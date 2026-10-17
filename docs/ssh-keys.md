# SSH keys for hosts

`host add` used to stop at "can't SSH to DEST without a password; run `ssh-copy-id`". A
fresh Mac with Remote Login on only takes a password, so every first `host add` ended there.
It now sets up a key itself, once per host, when it runs in a terminal.

## What `host add` does when the key isn't there

1. `ssh -o BatchMode=yes DEST true`. If that works (your own key, an agent, a config you
   wrote), nothing below happens.
2. If ssh says `Permission denied` and stdin is a terminal:
   - Creates `~/.rekcod/ssh/id_ed25519` the first time (ed25519, no passphrase: docker's
     SSH helper can't ask for one). The same key serves every host.
   - Runs `ssh-copy-id -i ~/.rekcod/ssh/id_ed25519.pub DEST`, which asks for the password
     on that Mac once and appends the key to its `~/.ssh/authorized_keys`.
   - Writes a block for the host to `~/.rekcod/ssh/config`:

     ```
     # rekcod host tailbus
     Host tailbus.local
       IdentityFile ~/.rekcod/ssh/id_ed25519
       IdentitiesOnly yes
     ```

   - Puts `Include ~/.rekcod/ssh/config` at the top of `~/.ssh/config`, between marker
     lines, once. It has to be in the user's config because docker's own ssh, which the
     `rekcod-NAME` context runs, reads only that.
   - Runs step 1 again; that has to pass.
3. Not a terminal, or another error (timeout, connection refused): the old message, plus
   "turn on Remote Login".

Hosts whose SSH already worked get no block and no key, and their config isn't touched.

## Keeping it in step

- `host refresh` (a LAN server found at a new name): the check that the new name holds the
  pinned host key offers the rekcod key too, and on success the block's `Host` line moves
  to the new name.
- `host rm NAME` removes its block.
- `rekcod uninstall` removes the `Include` lines from `~/.ssh/config`; without
  `--keep-data` the key goes with `~/.rekcod`. The public key stays in the other Mac's
  `authorized_keys`: the line ends in `rekcod@<this Mac's name>`.

## Dev builds bring their guest

`host add --server` copies this binary when the other Mac has none (or another version). A
release build downloads its guest there on first start. A dev build has no guest pinned, so
the other Mac's `rekcod start` and every app instance failed. Now, from a dev build,
`host add --server` also copies the local guest (`Image`, `root.img`, `VERSION`,
`initramfs`) to `~/.rekcod/guest` on the other Mac when its `VERSION` differs. That's
about 340 MB the first time and nothing after.

## Not covered

- A key with a passphrase, or one in the Secure Enclave. Docker's ssh would have to reach
  an agent; I'd rather not depend on one being there.
- Turning Remote Login on. That needs the other Mac's screen.
