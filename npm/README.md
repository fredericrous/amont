# npm packaging

Seven packages, all built from the artifacts `release.yaml` already produces for
the GitHub release. Nothing here is compiled; `scripts/npm-pack.sh` unpacks the
release tarballs and lays the trees out.

```
npm/
  amont/                 the package people depend on
    package.json.in      optionalDependencies on all six platform packages
    bin/amont.js         resolves the platform binary and execs it
    README.md
  platform/
    package.json.in      one template, six substitutions
```

## Why optional dependencies and not a postinstall

The alternative is one package with a `postinstall` that downloads the right
tarball. It is simpler to publish and worse to depend on:

- `npm ci --ignore-scripts` is a normal hardening choice and a security default
  in several CI images. A postinstall install silently produces a package with
  no binary in it, and the failure surfaces later as "amont: not found" from a
  git hook.
- It needs the network at install time, so a GitHub outage becomes a broken
  `npm install` for everyone.
- It cannot be cached or mirrored as an ordinary package, which matters behind a
  pull-through registry.

`os` / `cpu` / `libc` on each platform package let npm and pnpm resolve exactly
one of the six and skip the rest, with no code running at install time at all.

## Why `bin/amont.js` is not on the hook path

The `bin` a package manager links has to live inside the package, so it is a JS
file, and node's start-up is ~30ms — on a tool whose whole posture is that it
runs on every commit.

It never pays that. `amont init` bakes `current_exe()`, which is the **native**
binary inside the platform package, straight into `.git/hooks`. The JS wrapper
runs exactly once, during `prepare`; every commit afterwards execs the binary
directly. `init_writes_the_four_shims_and_bakes_the_running_binary` in
`crates/amont/tests/it/init.rs` is what keeps that true.
