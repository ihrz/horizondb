/*
 * Update and build steps shared by pm2.config.cjs (on every start) and
 * scripts/deploy.cjs (while the running server keeps serving).
 *
 * The server always runs dist/horizondb, never target/release/horizondb:
 * a build only replaces it once it succeeded (copy to a temporary file, then
 * rename), so a failed or interrupted build leaves the previous binary in
 * place. dist/horizondb.commit records the commit that binary was built
 * from: when HEAD did not move, nothing is rebuilt.
 */

const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const ROOT = path.resolve(__dirname, "..");
const EXE = process.platform === "win32" ? ".exe" : "";
const BINARY = path.join(ROOT, "dist", `horizondb${EXE}`);
const STAMP = path.join(ROOT, "dist", "horizondb.commit");
const BUILT = path.join(
	process.env.CARGO_TARGET_DIR ? path.resolve(ROOT, process.env.CARGO_TARGET_DIR) : path.join(ROOT, "target"),
	"release",
	`horizondb${EXE}`
);

// pm2 often starts with a minimal PATH: make rustup's default location visible.
const CARGO_BIN = path.join(os.homedir(), ".cargo", "bin");
const ENV = {
	...process.env,
	PATH: [CARGO_BIN, process.env.PATH].filter(Boolean).join(path.delimiter)
};

function log(message) {
	process.stdout.write(`[horizondb-release] ${message}\n`);
}

function run(file, args, options = {}) {
	execFileSync(file, args, { cwd: ROOT, stdio: "inherit", env: ENV, ...options });
}

function capture(file, args) {
	return execFileSync(file, args, { cwd: ROOT, env: ENV, encoding: "utf8" }).trim();
}

function headCommit() {
	return capture("git", ["rev-parse", "HEAD"]);
}

function builtCommit() {
	try {
		return fs.readFileSync(STAMP, "utf8").trim();
	} catch {
		return null;
	}
}

function hasBinary() {
	return fs.existsSync(BINARY);
}

/** Fast-forwards the checked-out branch; returns whether HEAD moved. */
function pull() {
	const before = headCommit();
	// --ff-only: a diverged or locally modified checkout is never merged or
	// rebased behind the operator's back; the pull fails and is reported.
	run("git", ["pull", "--ff-only"], { timeout: 120_000 });
	const after = headCommit();
	if (after !== before) log(`pulled ${before.slice(0, 9)} -> ${after.slice(0, 9)}`);
	return after !== before;
}

/** Whether dist/horizondb is missing or older than HEAD. */
function needsBuild() {
	return !hasBinary() || builtCommit() !== headCommit();
}

/**
 * Builds the release binary and installs it into dist/. `lowPriority` runs
 * cargo under `nice`, for builds next to a server that is still serving.
 */
function build({ lowPriority = false } = {}) {
	const commit = headCommit();
	log(`building ${commit.slice(0, 9)} (cargo build --release)`);
	const started = Date.now();
	const cargo = ["build", "--release"];
	if (lowPriority && process.platform !== "win32") {
		run("nice", ["-n", "10", "cargo", ...cargo], { timeout: 3_600_000 });
	} else {
		run("cargo", cargo, { timeout: 3_600_000 });
	}

	fs.mkdirSync(path.dirname(BINARY), { recursive: true });
	const temporary = `${BINARY}.new`;
	fs.copyFileSync(BUILT, temporary);
	fs.chmodSync(temporary, 0o755);
	// rename() replaces the file atomically, even while the old one runs.
	fs.renameSync(temporary, BINARY);
	fs.writeFileSync(STAMP, `${commit}\n`);
	log(`installed ${path.relative(ROOT, BINARY)} in ${Math.round((Date.now() - started) / 1000)} s`);
}

module.exports = { ROOT, BINARY, ENV, log, pull, needsBuild, build, hasBinary, headCommit, builtCommit };
