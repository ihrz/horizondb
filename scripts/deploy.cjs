#!/usr/bin/env node
/*
 * Updates a HorizonDB started with `pm2 start pm2.config.cjs`, with the
 * server down only for the restart itself:
 *   1. `git pull --ff-only` (the server keeps serving);
 *   2. `cargo build --release` under `nice` when HEAD moved, installed into
 *      dist/ only if it succeeds (the server still keeps serving);
 *   3. `pm2 restart <name>`: the server flushes, stops, and the launcher
 *      starts the new binary right away (nothing left to build).
 * When there is nothing new, nothing is restarted. A failed pull or build
 * stops here and leaves the running server untouched.
 *
 * Usage: node scripts/deploy.cjs [--force] [--no-restart] [--name <pm2 app>]
 *   --force       rebuild and restart even when HEAD did not move
 *   --no-restart  pull and build only (restart yourself later)
 *   --name        pm2 app name (default: HORIZONDB_PM2_NAME or "db")
 */

const { execFileSync } = require("node:child_process");
const release = require("./release.cjs");

const args = process.argv.slice(2);
const force = args.includes("--force");
const restart = !args.includes("--no-restart");
const nameIndex = args.indexOf("--name");
const name =
	(nameIndex >= 0 && args[nameIndex + 1]) || process.env.HORIZONDB_PM2_NAME || "db";

function main() {
	release.pull();

	if (!force && !release.needsBuild()) {
		release.log(`up to date (${release.builtCommit().slice(0, 9)}), nothing to deploy`);
		return;
	}
	release.build({ lowPriority: true });

	if (!restart) {
		release.log(`built; restart with: pm2 restart ${name}`);
		return;
	}
	release.log(`restarting pm2 app "${name}"`);
	// No --update-env: the app keeps the environment it was started with.
	execFileSync("pm2", ["restart", name], {
		cwd: release.ROOT,
		stdio: "inherit",
		env: release.ENV
	});
	release.log("deployed");
}

try {
	main();
} catch (error) {
	release.log(`deploy failed, the running server was left as it is: ${error.message}`);
	process.exit(1);
}
