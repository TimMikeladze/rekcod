# GitHub Actions runner on rekcod

Runs a self-hosted runner as a container in the rekcod VM. Jobs get the VM's `dockerd`, so
`docker build`, `container:` jobs and `services:` work as on a Linux runner. Jobs run on
arm64.

```sh
cp .env.example .env        # set REPO_URL and ACCESS_TOKEN (fine-grained PAT, Administration: read/write)
rekcod compose up -d
rekcod compose logs -f      # wait for "Listening for Jobs"
```

Then copy `workflow/rekcod.yml` to `.github/workflows/` in that repo and push. Jobs pick the
runner with `runs-on: [self-hosted, rekcod]`.

`EPHEMERAL=true` makes the runner take one job, exit and re-register (`restart: unless-stopped`
brings it back). Job containers still share the VM with your own containers, so a job that runs
`docker system prune` prunes yours too.

Don't attach this to a public repo. Fork PRs could run code on your machine.

`rekcod stop` kills a running job. The runner comes back with the VM.
