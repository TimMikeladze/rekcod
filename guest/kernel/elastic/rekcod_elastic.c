// SPDX-License-Identifier: GPL-2.0
/*
 * rekcod elastic memory (docs/elastic-memory.md): a swap device on a virtio shared
 * memory region that the host backs with its own memory. Chunks the guest stops using
 * are handed back to the host (RELEASE), which returns them to macOS; a chunk is taken
 * back (REUSE) before anything is written to it again.
 *
 * The region is never executed from or used as RAM: Virtualization.framework won't
 * run code from shared memory regions, and swap only ever copies pages in and out.
 */
#include <linux/module.h>
#include <linux/virtio.h>
#include <linux/virtio_config.h>
#include <linux/blkdev.h>
#include <linux/bio.h>
#include <linux/highmem.h>
#include <linux/io.h>
#include <linux/scatterlist.h>
#include <linux/workqueue.h>
#include <linux/vmalloc.h>
#include <linux/slab.h>

/* rekcod_proto: ELASTIC_DEVICE_ID, ELASTIC_OP_* */
#define VIRTIO_ID_REKCOD_ELASTIC 63
#define OP_RELEASE 1
#define OP_REUSE 2
#define MAX_BATCH 256
#define RELEASE_EVERY (5 * HZ)

struct elastic_config {
	__le64 chunk_size;
	__le32 ready;
	__le32 reserved;
};

struct elastic_req {
	__le32 op;
	__le32 count;
	__le32 chunk[MAX_BATCH];
};

enum { CHUNK_ACTIVE, CHUNK_RELEASED };

struct elastic {
	struct virtio_device *vdev;
	struct virtqueue *vq;
	spinlock_t vq_lock;
	void *mem;
	u64 size, chunk, nchunks;

	/* Under lock: which pages hold swap data, and per chunk the counts and state. */
	spinlock_t lock;
	unsigned long *pages;
	u32 *used;
	u32 *inflight;
	u8 *state, *written, *idle;

	/* One request on the queue at a time; held across a RELEASE so no REUSE overtakes it. */
	struct mutex req_lock;
	struct completion done;
	struct elastic_req *req;
	__le32 *status;

	/* jiffies of the last write: nothing is released while the guest is swapping. */
	unsigned long last_write;

	struct gendisk *disk;
	struct delayed_work release_work;
	struct work_struct ready_work;
	bool disk_added;
};

static void elastic_vq_done(struct virtqueue *vq)
{
	struct elastic *e = vq->vdev->priv;
	unsigned long flags;
	unsigned int len;

	spin_lock_irqsave(&e->vq_lock, flags);
	while (virtqueue_get_buf(vq, &len))
		complete(&e->done);
	spin_unlock_irqrestore(&e->vq_lock, flags);
}

/* Caller holds req_lock. */
static int elastic_send(struct elastic *e, u32 op, const u32 *chunks, u32 n)
{
	struct scatterlist out, in, *sgs[2] = { &out, &in };
	unsigned long flags;
	u32 i;
	int ret;

	e->req->op = cpu_to_le32(op);
	e->req->count = cpu_to_le32(n);
	for (i = 0; i < n; i++)
		e->req->chunk[i] = cpu_to_le32(chunks[i]);
	*e->status = cpu_to_le32(~0U);
	sg_init_one(&out, e->req, 8 + 4 * n);
	sg_init_one(&in, e->status, sizeof(*e->status));
	reinit_completion(&e->done);
	spin_lock_irqsave(&e->vq_lock, flags);
	ret = virtqueue_add_sgs(e->vq, sgs, 1, 1, e, GFP_ATOMIC);
	if (!ret)
		virtqueue_kick(e->vq);
	spin_unlock_irqrestore(&e->vq_lock, flags);
	if (ret)
		return ret;
	wait_for_completion(&e->done);
	return le32_to_cpu(*e->status) ? -EIO : 0;
}

/* Before copying into a chunk: makes sure the host has it, and counts us in. */
static int elastic_write_begin(struct elastic *e, u64 c)
{
	u32 chunk = c;
	int ret = 0;

	spin_lock(&e->lock);
	if (e->state[c] == CHUNK_ACTIVE) {
		e->inflight[c]++;
		spin_unlock(&e->lock);
		return 0;
	}
	spin_unlock(&e->lock);

	mutex_lock(&e->req_lock);
	spin_lock(&e->lock);
	if (e->state[c] == CHUNK_RELEASED) {
		spin_unlock(&e->lock);
		ret = elastic_send(e, OP_REUSE, &chunk, 1);
		spin_lock(&e->lock);
		if (!ret)
			e->state[c] = CHUNK_ACTIVE;
	}
	if (!ret)
		e->inflight[c]++;
	spin_unlock(&e->lock);
	mutex_unlock(&e->req_lock);
	return ret;
}

static void elastic_write_end(struct elastic *e, u64 off, u32 len)
{
	u64 c = off / e->chunk, p;

	spin_lock(&e->lock);
	for (p = off >> PAGE_SHIFT; p < (off + len) >> PAGE_SHIFT; p++)
		if (!__test_and_set_bit(p, e->pages))
			e->used[c]++;
	e->written[c] = 1;
	e->idle[c] = 0;
	e->inflight[c]--;
	spin_unlock(&e->lock);
	WRITE_ONCE(e->last_write, jiffies);
}

static void elastic_discard(struct elastic *e, u64 off, u64 len)
{
	u64 p, end = min(off + len, e->size) >> PAGE_SHIFT;

	spin_lock(&e->lock);
	for (p = DIV_ROUND_UP(off, PAGE_SIZE); p < end; p++)
		if (__test_and_clear_bit(p, e->pages))
			e->used[(p << PAGE_SHIFT) / e->chunk]--;
	spin_unlock(&e->lock);
}

static void elastic_submit_bio(struct bio *bio)
{
	struct elastic *e = bio->bi_bdev->bd_disk->private_data;
	u64 off = bio->bi_iter.bi_sector << SECTOR_SHIFT;
	struct bvec_iter iter;
	struct bio_vec bv;

	switch (bio_op(bio)) {
	case REQ_OP_DISCARD:
		elastic_discard(e, off, bio->bi_iter.bi_size);
		bio_endio(bio);
		return;
	case REQ_OP_FLUSH:
		bio_endio(bio);
		return;
	case REQ_OP_READ:
	case REQ_OP_WRITE:
		break;
	default:
		bio->bi_status = BLK_STS_NOTSUPP;
		bio_endio(bio);
		return;
	}
	if (off + bio->bi_iter.bi_size > e->size) {
		bio_io_error(bio);
		return;
	}
	bio_for_each_segment(bv, bio, iter) {
		void *buf = bvec_kmap_local(&bv);

		if (op_is_write(bio_op(bio))) {
			/* Segments are 4K-aligned (logical block size), so never span chunks. */
			if (elastic_write_begin(e, off / e->chunk)) {
				kunmap_local(buf);
				bio_io_error(bio);
				return;
			}
			memcpy(e->mem + off, buf, bv.bv_len);
			elastic_write_end(e, off, bv.bv_len);
		} else {
			memcpy(buf, e->mem + off, bv.bv_len);
		}
		kunmap_local(buf);
		off += bv.bv_len;
	}
	bio_endio(bio);
}

static const struct block_device_operations elastic_fops = {
	.owner = THIS_MODULE,
	.submit_bio = elastic_submit_bio,
};

/*
 * Hands back chunks that were written, then stayed empty for a whole interval. Skipped
 * while the guest is still swapping: a chunk emptied mid-burst is usually written again
 * soon, and taking it back costs a round trip plus fresh pages on the Mac.
 */
static void elastic_release(struct work_struct *work)
{
	struct elastic *e = container_of(to_delayed_work(work), struct elastic, release_work);
	static u32 batch[MAX_BATCH];
	u64 c = time_before(jiffies, READ_ONCE(e->last_write) + RELEASE_EVERY) ? e->nchunks : 0;

	while (c < e->nchunks) {
		u32 n = 0;

		mutex_lock(&e->req_lock);
		spin_lock(&e->lock);
		for (; c < e->nchunks && n < MAX_BATCH; c++) {
			if (e->state[c] != CHUNK_ACTIVE || !e->written[c] || e->used[c] || e->inflight[c])
				continue;
			if (!e->idle[c]) {
				e->idle[c] = 1;
				continue;
			}
			e->state[c] = CHUNK_RELEASED;
			e->written[c] = 0;
			batch[n++] = c;
		}
		spin_unlock(&e->lock);
		if (n && elastic_send(e, OP_RELEASE, batch, n)) {
			u32 i;

			/* The host still has them, or has nothing mapped there: keep them ours. */
			dev_warn_ratelimited(&e->vdev->dev, "release of %u chunks failed\n", n);
			spin_lock(&e->lock);
			for (i = 0; i < n; i++) {
				e->state[batch[i]] = CHUNK_ACTIVE;
				e->written[batch[i]] = 1;
			}
			spin_unlock(&e->lock);
		}
		mutex_unlock(&e->req_lock);
	}
	schedule_delayed_work(&e->release_work, RELEASE_EVERY);
}

static void elastic_add_disk(struct work_struct *work)
{
	struct elastic *e = container_of(work, struct elastic, ready_work);
	struct queue_limits lim = {
		.logical_block_size = PAGE_SIZE,
		.physical_block_size = PAGE_SIZE,
		.max_hw_discard_sectors = UINT_MAX,
		.discard_granularity = PAGE_SIZE,
		.features = BLK_FEAT_SYNCHRONOUS,
	};
	int ret;

	if (e->disk_added || !virtio_cread32(e->vdev, offsetof(struct elastic_config, ready)))
		return;
	e->disk = blk_alloc_disk(&lim, NUMA_NO_NODE);
	if (IS_ERR(e->disk)) {
		dev_err(&e->vdev->dev, "alloc disk: %ld\n", PTR_ERR(e->disk));
		e->disk = NULL;
		return;
	}
	strscpy(e->disk->disk_name, "rswap", DISK_NAME_LEN);
	e->disk->fops = &elastic_fops;
	e->disk->private_data = e;
	e->disk->flags |= GENHD_FL_NO_PART;
	set_capacity(e->disk, e->size >> SECTOR_SHIFT);
	ret = add_disk(e->disk);
	if (ret) {
		dev_err(&e->vdev->dev, "add disk: %d\n", ret);
		put_disk(e->disk);
		e->disk = NULL;
		return;
	}
	e->disk_added = true;
	schedule_delayed_work(&e->release_work, RELEASE_EVERY);
	dev_info(&e->vdev->dev, "%llu MiB elastic memory as /dev/rswap\n", e->size >> 20);
}

static void elastic_config_changed(struct virtio_device *vdev)
{
	struct elastic *e = vdev->priv;

	schedule_work(&e->ready_work);
}

static int elastic_probe(struct virtio_device *vdev)
{
	struct virtio_shm_region shm;
	struct elastic *e;
	int ret = -ENOMEM;

	e = devm_kzalloc(&vdev->dev, sizeof(*e), GFP_KERNEL);
	if (!e)
		return -ENOMEM;
	vdev->priv = e;
	e->vdev = vdev;
	spin_lock_init(&e->vq_lock);
	spin_lock_init(&e->lock);
	mutex_init(&e->req_lock);
	init_completion(&e->done);
	INIT_DELAYED_WORK(&e->release_work, elastic_release);
	INIT_WORK(&e->ready_work, elastic_add_disk);

	if (!virtio_get_shm_region(vdev, &shm, 0))
		return -ENODEV;
	e->chunk = virtio_cread64(vdev, offsetof(struct elastic_config, chunk_size));
	if (!e->chunk || e->chunk % PAGE_SIZE || shm.len % e->chunk)
		return -EINVAL;
	e->size = shm.len;
	e->nchunks = shm.len / e->chunk;
	e->mem = devm_memremap(&vdev->dev, shm.addr, shm.len, MEMREMAP_WB);
	if (IS_ERR(e->mem))
		return PTR_ERR(e->mem);

	e->pages = devm_bitmap_zalloc(&vdev->dev, e->size >> PAGE_SHIFT, GFP_KERNEL);
	e->used = devm_kcalloc(&vdev->dev, e->nchunks, sizeof(u32), GFP_KERNEL);
	e->inflight = devm_kcalloc(&vdev->dev, e->nchunks, sizeof(u32), GFP_KERNEL);
	e->state = devm_kcalloc(&vdev->dev, e->nchunks, 1, GFP_KERNEL);
	e->written = devm_kcalloc(&vdev->dev, e->nchunks, 1, GFP_KERNEL);
	e->idle = devm_kcalloc(&vdev->dev, e->nchunks, 1, GFP_KERNEL);
	e->req = devm_kzalloc(&vdev->dev, sizeof(*e->req), GFP_KERNEL);
	e->status = devm_kzalloc(&vdev->dev, sizeof(*e->status), GFP_KERNEL);
	if (!e->pages || !e->used || !e->inflight || !e->state || !e->written || !e->idle || !e->req || !e->status)
		return ret;

	e->vq = virtio_find_single_vq(vdev, elastic_vq_done, "requests");
	if (IS_ERR(e->vq))
		return PTR_ERR(e->vq);
	virtio_device_ready(vdev);
	/* The host sets ready once its memory is mapped in; until then, wait for config_changed. */
	schedule_work(&e->ready_work);
	return 0;
}

static void elastic_remove(struct virtio_device *vdev)
{
	struct elastic *e = vdev->priv;

	cancel_work_sync(&e->ready_work);
	cancel_delayed_work_sync(&e->release_work);
	if (e->disk_added) {
		del_gendisk(e->disk);
		put_disk(e->disk);
	}
	virtio_reset_device(vdev);
	vdev->config->del_vqs(vdev);
}

static const struct virtio_device_id elastic_ids[] = {
	{ VIRTIO_ID_REKCOD_ELASTIC, VIRTIO_DEV_ANY_ID },
	{ 0 },
};

static struct virtio_driver elastic_driver = {
	.driver.name = "rekcod_elastic",
	.id_table = elastic_ids,
	.probe = elastic_probe,
	.remove = elastic_remove,
	.config_changed = elastic_config_changed,
};

module_virtio_driver(elastic_driver);
MODULE_DEVICE_TABLE(virtio, elastic_ids);
MODULE_DESCRIPTION("rekcod elastic memory");
MODULE_LICENSE("GPL");
