// SPDX-License-Identifier: GPL-2.0
/*
 * rekcod: host-driven invalidation for virtiofs mounts with long cache TTLs.
 *
 * Write lines to /sys/module/fuse/parameters/rekcod_notify:
 *   /a/b/c                      the host changed, created or deleted c: drop c's
 *                               attributes and page cache, invalidate the name in b
 *                               (positive or negative), and raise the inotify event
 *                               a local change would have raised
 *   /a/b/c\tSIZE\tMS\tMNS\tCS\tCNS  same, but skipped when the guest's cached
 *                               size/mtime/ctime already match the host's (echoes of
 *                               the guest's own writes, files the guest just read)
 *   /a/b/c\t-                   same, skipped when the guest already caches c as absent
 *   /a/b/                       invalidate everything cached below b
 *
 * Only paths whose parent is already in the dcache are touched (rekcod_walk),
 * so changes to directories the guest never looked at cost nothing.
 */

#include "fuse_i.h"

#include <linux/fsnotify.h>
#include <linux/iversion.h>
#include <linux/magic.h>
#include <linux/moduleparam.h>
#include <linux/namei.h>
#include <linux/slab.h>

/* jiffies64 of the last host-driven invalidation; see virtio_fs.c */
atomic64_t rekcod_last_inval = ATOMIC64_INIT(0);
EXPORT_SYMBOL_GPL(rekcod_last_inval);

static bool rekcod_is_fuse(struct dentry *d)
{
	return d->d_sb->s_magic == FUSE_SUPER_MAGIC;
}

static void rekcod_dir_changed(struct inode *dir)
{
	fuse_invalidate_attr(dir);
	inode_maybe_inc_iversion(dir, false);
}

static bool rekcod_watched(struct inode *inode)
{
	return inode && READ_ONCE(inode->i_fsnotify_marks);
}

/*
 * Resolve an absolute path through the dcache only: the first component via
 * kern_path (the share's mount point, e.g. /Users), the rest with
 * d_hash_and_lookup. Unlike LOOKUP_CACHED this accepts dentries whose TTL
 * expired: they are still what containers see, so they must be invalidated.
 */
static int rekcod_walk(const char *path, struct path *out)
{
	const char *rest = strchr(path + 1, '/'), *comp;
	size_t n = rest ? (size_t)(rest - path) : strlen(path);
	struct dentry *child;
	struct path p;
	char first[NAME_MAX + 2];
	struct qstr q;
	int err;

	if (path[0] != '/' || n >= sizeof(first))
		return -EINVAL;
	memcpy(first, path, n);
	first[n] = '\0';
	err = kern_path(first, LOOKUP_FOLLOW, &p);
	if (err)
		return err;
	if (!rekcod_is_fuse(p.dentry)) {
		path_put(&p);
		return -EINVAL;
	}
	while (rest) {
		while (*rest == '/')
			rest++;
		if (!*rest)
			break;
		comp = rest;
		rest = strchr(comp, '/');
		q = (struct qstr)QSTR_INIT(comp, rest ? (u32)(rest - comp) : (u32)strlen(comp));
		child = d_hash_and_lookup(p.dentry, &q);
		if (IS_ERR_OR_NULL(child) || d_really_is_negative(child) || !d_is_dir(child)) {
			if (!IS_ERR_OR_NULL(child))
				dput(child);
			path_put(&p);
			return -ENOENT;
		}
		dput(p.dentry);
		p.dentry = child;
	}
	*out = p;
	return 0;
}

static void rekcod_tree(char *path)
{
	struct path p;

	if (rekcod_walk(path, &p))
		return;
	if (rekcod_is_fuse(p.dentry) && d_is_dir(p.dentry)) {
		atomic64_set(&rekcod_last_inval, get_jiffies_64());
		shrink_dcache_parent(p.dentry);
		rekcod_dir_changed(d_inode(p.dentry));
	}
	path_put(&p);
}

/*
 * macOS shares are usually case-insensitive: once "Foo" is created, renamed or
 * removed, any cached dentry for "foo"/"FOO" (a negative from an earlier failed
 * lookup, or a positive alias of the old file) is wrong for up to the cache TTL.
 * Unhash every case variant of @name in @parent other than @name itself; the
 * next access looks it up again. ASCII folding only, like most tools expect.
 */
void rekcod_drop_case_aliases(struct dentry *parent, const struct qstr *name)
{
	struct dentry *child;

	spin_lock(&parent->d_lock);
	hlist_for_each_entry(child, &parent->d_children, d_sib) {
		if (child->d_name.len != name->len)
			continue;
		spin_lock_nested(&child->d_lock, DENTRY_D_LOCK_NESTED);
		if (!d_unhashed(child) && !d_mountpoint(child) &&
		    memcmp(child->d_name.name, name->name, name->len) &&
		    !strncasecmp(child->d_name.name, name->name, name->len))
			__d_drop(child);
		spin_unlock(&child->d_lock);
	}
	spin_unlock(&parent->d_lock);
}

struct rekcod_stat {
	bool known;	/* the host sent its view of the file */
	bool missing;	/* ... and the file does not exist */
	u64 size;
	struct timespec64 mtime, ctime;
};

static void rekcod_parse_stat(char *fields, struct rekcod_stat *st)
{
	char *f[5];
	unsigned long long v[5];
	int i;

	if (!fields)
		return;
	if (!strcmp(fields, "-")) {
		st->known = st->missing = true;
		return;
	}
	for (i = 0; i < 5; i++) {
		f[i] = strsep(&fields, "\t");
		if (!f[i] || kstrtoull(f[i], 10, &v[i]))
			return;
	}
	st->known = true;
	st->size = v[0];
	st->mtime = (struct timespec64){ .tv_sec = v[1], .tv_nsec = v[2] };
	st->ctime = (struct timespec64){ .tv_sec = v[3], .tv_nsec = v[4] };
}

static bool rekcod_cache_matches(struct dentry *old, const struct rekcod_stat *st)
{
	struct inode *inode;

	if (!st->known || !old)
		return false;
	if (st->missing)
		return d_really_is_negative(old);
	if (d_really_is_negative(old))
		return false;
	inode = d_inode(old);
	return i_size_read(inode) == st->size &&
	       timespec64_equal(&st->mtime, &(struct timespec64){ inode_get_mtime_sec(inode), inode_get_mtime_nsec(inode) }) &&
	       timespec64_equal(&st->ctime, &(struct timespec64){ inode_get_ctime_sec(inode), inode_get_ctime_nsec(inode) });
}

static void rekcod_one(char *path, const struct rekcod_stat *st)
{
	struct path parent;
	struct inode *dir, *old_inode = NULL;
	struct dentry *old, *now;
	struct fuse_conn *fc;
	struct qstr name;
	char *slash = strrchr(path, '/');
	bool existed, exists;

	if (!slash || !slash[1])
		return;
	*slash = '\0';
	name = (struct qstr)QSTR_INIT(slash + 1, strlen(slash + 1));
	if (rekcod_walk(slash == path ? "/" : path, &parent))
		return;
	if (!rekcod_is_fuse(parent.dentry) || !d_is_dir(parent.dentry))
		goto out;

	dir = d_inode(parent.dentry);
	fc = get_fuse_conn(dir);

	old = d_hash_and_lookup(parent.dentry, &name);
	if (IS_ERR(old))
		old = NULL;
	if (rekcod_cache_matches(old, st)) {
		dput(old);
		goto out;
	}
	rekcod_drop_case_aliases(parent.dentry, &name);
	existed = old && d_really_is_positive(old);
	if (existed)
		old_inode = igrab(d_inode(old));
	atomic64_set(&rekcod_last_inval, get_jiffies_64());

	/*
	 * A directory that still exists only changed its contents (its mtime moved
	 * because an entry was added or removed). Keep its dentry: unhashing it
	 * would cut the cached path the next events for its children walk, while
	 * containers keep using it through their bind mounts. Drop its attributes
	 * and listing cache instead.
	 */
	if (existed && d_is_dir(old) && st->known && !st->missing) {
		down_read(&fc->killsb);
		fuse_reverse_inval_inode(fc, get_node_id(old_inode), 0, 0);
		up_read(&fc->killsb);
		rekcod_dir_changed(old_inode);
		rekcod_dir_changed(dir);
		iput(old_inode);
		dput(old);
		goto out;
	}

	/* Same locking as the FUSE_NOTIFY_INVAL_* handlers in dev.c. */
	down_read(&fc->killsb);
	if (old_inode)
		fuse_reverse_inval_inode(fc, get_node_id(old_inode), 0, 0);
	fuse_reverse_inval_entry(fc, get_node_id(dir), 0, &name, 0);
	up_read(&fc->killsb);
	rekcod_dir_changed(dir);

	/* Re-look-up only when someone watches; otherwise the next access does it. */
	if (rekcod_watched(dir) || rekcod_watched(old_inode)) {
		now = lookup_one_unlocked(&nop_mnt_idmap, name.name, parent.dentry, name.len);
		exists = !IS_ERR(now) && d_really_is_positive(now);
		if (exists && (!existed || d_inode(now) != old_inode))
			fsnotify_create(dir, now);
		else if (exists)
			fsnotify_dentry(now, FS_MODIFY | FS_CLOSE_WRITE);
		else if (existed)
			fsnotify_delete(dir, old_inode, old);
		if (!IS_ERR(now))
			dput(now);
	}

	iput(old_inode);
	dput(old);
out:
	path_put(&parent);
}

static int rekcod_notify_set(const char *val, const struct kernel_param *kp)
{
	char *buf = kstrdup(val, GFP_KERNEL);
	char *cursor = buf, *line;
	size_t len;

	if (!buf)
		return -ENOMEM;
	while ((line = strsep(&cursor, "\n")) != NULL) {
		struct rekcod_stat st = {};
		char *fields = strchr(line, '\t');

		if (fields) {
			*fields++ = '\0';
			rekcod_parse_stat(fields, &st);
		}
		len = strlen(line);
		if (len < 2 || line[0] != '/')
			continue;
		if (line[len - 1] == '/') {
			line[len - 1] = '\0';
			rekcod_tree(line);
		} else {
			rekcod_one(line, &st);
		}
	}
	kfree(buf);
	return 0;
}

static const struct kernel_param_ops rekcod_notify_ops = {
	.set = rekcod_notify_set,
};
module_param_cb(rekcod_notify, &rekcod_notify_ops, NULL, 0200);
MODULE_PARM_DESC(rekcod_notify, "Host-changed paths to invalidate (rekcod)");
