#!/usr/bin/env node
// The Claude Code guard. See ./native.js for how the platform binary is found.
//
// Exposed as an npm bin because this is the binary a repository installs
// per-project alongside amont itself; `amont-fleet` is deliberately not, being
// a dashboard you run interactively from a real install rather than from a
// project's node_modules.
require("./native.js").become("amont-agent");
