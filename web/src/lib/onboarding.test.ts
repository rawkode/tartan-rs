import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "vitest";
import { moduleCue, pushCommands, tartanCue } from "./onboarding.ts";
import { cueRegistry } from "./settings.ts";

test("the module names the repository and pins the extension modules", () => {
	const text = moduleCue("acme", "api");
	expect(text).toContain('module: "tartan.example/acme/api"');
	expect(text).toContain('"tartan.rawkode.academy/issues@v0": v: "v0.6.0"');
	expect(text).toContain('"tartan.rawkode.academy/kanban@v0": v: "v0.3.0"');
});

test("the configuration binds Kanban to the repository's Issues entry", () => {
	const text = tartanCue();
	expect(text).toContain('#issues: extensions."issues.tartan.rawkode.academy"');
	expect(text).toContain("columns: props.status");
});

test("the push uses the Git host token and the registry", () => {
	const text = pushCommands({
		cloneUrl: "http://127.0.0.1:9200/acme/api.git",
		repo: "api",
		registry: cueRegistry("http://127.0.0.1:8080"),
		gitToken: "tok",
	});
	expect(text).toContain("export CUE_REGISTRY=127.0.0.1:8080+insecure");
	expect(text).toContain('git -c http.extraHeader="Authorization: Bearer tok" push origin HEAD:main');
	expect(cueRegistry("https://forge.example.com")).toBe("forge.example.com");
});

// With CUE_BIN and CUE_REGISTRY pointing at a registry serving the types, Issues and Kanban
// modules, the example must evaluate natively, exactly as the forge evaluates it.
test.runIf(process.env.CUE_BIN && process.env.CUE_REGISTRY)("the example evaluates with cue", () => {
	const dir = mkdtempSync(join(tmpdir(), "tartan-onboarding-"));
	try {
		mkdirSync(join(dir, "cue.mod"));
		writeFileSync(join(dir, "cue.mod", "module.cue"), moduleCue("acme", "api"));
		writeFileSync(join(dir, "tartan.cue"), tartanCue());
		execFileSync(process.env.CUE_BIN!, ["fmt", "--check", ".", "cue.mod/module.cue"], { cwd: dir });
		const out = JSON.parse(
			execFileSync(process.env.CUE_BIN!, ["export", ".", "-e", "extensions"], { cwd: dir }).toString(),
		) as Record<string, { id: string; settings: { boards?: Record<string, { columns: { name: string } }> } }>;
		expect(out["issues.tartan.rawkode.academy"]!.id).toBe("issues.tartan.rawkode.academy");
		expect(out["kanban.tartan.rawkode.academy"]!.settings.boards!.triage!.columns.name).toBe("status");
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});
