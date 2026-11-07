/*
 * pm2 entry point for HorizonDB (`pm2 start pm2.config.cjs`).
 *
 * This file is both the pm2 config and the launcher pm2 runs:
 * - loaded by the pm2 CLI, it only exports the app config below;
 * - run by pm2 as the app (HORIZONDB_PM2_LAUNCH=1), on every start and every
 *   automatic restart it:
 *     1. `git pull --ff-only` the checked-out branch;
 *     2. builds the release binary when HEAD differs from the one installed
 *        in dist/ (scripts/release.cjs), and installs it atomically;
 *     3. runs dist/horizondb with the config file, signals forwarded, so a
 *        stop or restart lets the server flush its write-behind buffer.
 * A failed pull or build is logged and the previously installed binary
 * starts. Only a checkout that never built anything fails to start.
 *
 * A release build takes minutes (LTO, one codegen unit) and the server is
 * down while this launcher builds. To update without that downtime, build
 * first and restart after: `node scripts/deploy.cjs` (see its header).
 *
 * Environment:
 *   HORIZONDB_CONFIG        config file (default: config.toml in this folder)
 *   HORIZONDB_PM2_NAME      pm2 app name (default: db)
 *   HORIZONDB_SKIP_UPDATE=1 start the installed binary without pull or build
 *   RUST_LOG                log filter (default: info)
 */

const { spawn } = require("node:child_process");
const path = require("node:path");

const ROOT = __dirname;

const config = {
	name: process.env.HORIZONDB_PM2_NAME || "db",
	cwd: ROOT,
	script: __filename,
	interpreter: "node",
	env: {
		HORIZONDB_PM2_LAUNCH: "1",
		HORIZONDB_CONFIG: process.env.HORIZONDB_CONFIG || path.join(ROOT, "config.toml")
	},
	autorestart: true,
	// A launcher that builds for minutes is "up" for pm2; crash loops of the
	// server itself are what these bound.
	min_uptime: 30_000,
	max_restarts: 10,
	restart_delay: 5000,
	// The server bounds its own shutdown (final flush + pool close) to 10 s:
	// leave it that time before pm2 resorts to SIGKILL.
	kill_timeout: 20_000,
	time: true
};

module.exports = config;

function launch() {
	const release = require("./scripts/release.cjs");
	const { log } = release;

	const step = (label, action) => {
		try {
			action();
		} catch (error) {
			log(`${label} failed, continuing with the installed binary: ${error.message}`);
		}
	};

	if (process.env.HORIZONDB_SKIP_UPDATE === "1") {
		log("HORIZONDB_SKIP_UPDATE=1: no pull, no build");
	} else {
		step("git pull", () => release.pull());
		if (release.needsBuild()) {
			if (release.hasBinary()) step("build", () => release.build());
			// Nothing to fall back on: let the failure reach pm2.
			else release.build();
		} else {
			log(`dist/horizondb is up to date (${release.builtCommit().slice(0, 9)})`);
		}
	}
	if (!release.hasBinary()) {
		throw new Error("dist/horizondb is missing: run `cargo build --release` once");
	}

	const configPath = process.env.HORIZONDB_CONFIG || path.join(ROOT, "config.toml");
	log(`starting ${path.relative(ROOT, release.BINARY)} ${configPath}`);
	const server = spawn(release.BINARY, [configPath], {
		cwd: ROOT,
		stdio: "inherit",
		env: { ...release.ENV, RUST_LOG: process.env.RUST_LOG || "info" }
	});
	for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
		process.on(signal, () => server.kill(signal));
	}
	server.on("error", (error) => {
		log(`could not start the server: ${error.message}`);
		process.exit(1);
	});
	server.on("exit", (code, signal) => {
		process.exit(code ?? (signal ? 1 : 0));
	});
}

if (process.env.HORIZONDB_PM2_LAUNCH === "1") launch();
