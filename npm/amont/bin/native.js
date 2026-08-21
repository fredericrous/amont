// Find the native binary npm installed for this platform, and become it.
//
// This wrapper exists only because a package manager can link a `bin` that
// lives inside the package, and the native executable lives in a DIFFERENT
// package — one of six, selected by npm from `os`/`cpu`/`libc`.
//
// It is not on the hook path. `amont init` bakes `current_exe()` — the native
// binary this spawns, not this file — into `.git/hooks`, so the ~30ms of node
// start-up below is paid once during `prepare` and never again on a commit.
// See npm/README.md.
//
// Parameterised by binary name rather than copied per binary. Everything below
// — pnpm's non-hoisting, the libc fallback, spawn-versus-exists, signal exit
// codes — was learned once and is worth exactly one copy; a second wrapper that
// duplicated it would be a second place for those lessons to rot.

const { spawnSync } = require("node:child_process");
const { existsSync } = require("node:fs");

// The same six targets `release.yaml` builds, spelled the way npm spells them.
// `libc` is why linux-x64 appears twice: a musl host cannot run the glibc build,
// and npm will install only the package whose `libc` matches.
const PACKAGES = {
  "darwin arm64": "amont-darwin-arm64",
  "darwin x64": "amont-darwin-x64",
  "linux arm64": "amont-linux-arm64-gnu",
  "linux x64": ["amont-linux-x64-gnu", "amont-linux-x64-musl"],
  "win32 x64": "amont-win32-x64",
};

function candidates() {
  const found = PACKAGES[`${process.platform} ${process.arch}`];
  if (!found) return [];
  return Array.isArray(found) ? found : [found];
}

// `require.resolve` rather than a hand-built `../amont-<target>/bin/…` path.
// pnpm does not hoist — the real package sits under `node_modules/.pnpm/` — so a
// path assembled from `__dirname` is correct under npm and wrong under pnpm and
// yarn. Node's own resolver knows where the dependency actually is.
function binaryOf(pkg, name) {
  const exe = process.platform === "win32" ? `${name}.exe` : name;
  try {
    const p = require.resolve(`${pkg}/bin/${exe}`);
    if (existsSync(p)) return p;
  } catch {
    // Not installed: either the wrong libc for this host, or an
    // `--ignore-scripts`-style install that skipped optional deps.
  }
  return null;
}

// Existence is not runnability, so the loop is over SPAWNS, not paths. With a
// package manager that ignores the `libc` field — yarn classic does, and
// `npm install --force` is this file's own suggested remedy — BOTH linux-x64
// builds get installed, gnu listed first, and on a musl host the gnu binary
// fails at exec on the missing glibc loader while the right one sits a
// candidate later. Only a spawn-level failure (`result.error`) falls through:
// a binary that ran and exited non-zero has ANSWERED, and its exit code is the
// whole product of a hook runner.
function become(name) {
  const failures = [];
  for (const pkg of candidates()) {
    const binary = binaryOf(pkg, name);
    if (!binary) continue;
    // `spawnSync` with inherited stdio rather than `execFileSync`: this forwards
    // the child's exit CODE. It also keeps the child's stdin, which `amont run`
    // reads — and which `amont-agent hook` reads its whole payload from.
    const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
    if (result.error) {
      failures.push(`      ${binary}: ${result.error.message}`);
      continue;
    }
    // A signalled child has a null status. Report it the way a shell does, so
    // "killed by SIGINT" does not read as a clean exit 0.
    if (result.status === null && result.signal) {
      process.exit(128 + (require("node:os").constants.signals[result.signal] ?? 0));
    }
    process.exit(result.status ?? 1);
  }

  // Name the platform. "amont binary not found" sends people to reinstall;
  // "no build for linux/ppc64" tells them the actual answer, which is that they
  // want `cargo install amont` or the shell installer.
  const target = `${process.platform}/${process.arch}`;
  process.stderr.write(
    `${name}: no runnable native binary for ${target}.\n` +
      (failures.length ? `  Installed but could not run:\n${failures.join("\n")}\n` : "") +
      `  npm installs one of: ${Object.values(PACKAGES).flat().join(", ")}\n` +
      `  If your platform is not among them, build from source:\n` +
      `      cargo install amont\n` +
      `  If it is, the optional dependency did not install — try:\n` +
      `      npm install --force amont\n`,
  );
  process.exit(1);
}

module.exports = { become };
