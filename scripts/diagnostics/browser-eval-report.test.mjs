import assert from "node:assert/strict";
import { test } from "node:test";

const complete = () => ({ stats: { expected: 1, unexpected: 0, skipped: 0, flaky: 0 }, errors: [], suites: [{ title: "portable-core.spec.ts", specs: [{ id: "fixture-id", title: "fixture case", ok: true, tests: [{ projectName: "chromium", status: "expected", results: [{ status: "passed", attachments: ["portable-browser-evidence", "portable-browser-screenshot", "trace"].map(name => ({ name, path: `/tmp/evidence/${name}` })) }] }] }] }] });
test("portable matrix requires existing receipt and media cases in both tiers", async () => {
  const { requiredBrowserPaths } = await import("../evals/browser-report.mjs");
  for (const [tier, count] of [["core", 21], ["extended", 22]]) {
    const paths = requiredBrowserPaths(tier);
    assert.equal(paths.length, count);
    assert.equal(paths.filter(path => path[0] === "automatic-delivery.spec.ts").length, 2);
    assert.equal(paths.filter(path => path[0] === "media-recovery.spec.ts").length, 1);
    assert.equal(paths.filter(path => path[0] === "media-lifecycle.spec.ts").length, 3);
    const report = complete();
    report.stats.expected = count;
    report.suites = paths.map(([file, ...titles], index) => ({ title: file, specs: [{ ...complete().suites[0].specs[0], id: `case-${index}`, title: titles.at(-1), tests: [{ ...complete().suites[0].specs[0].tests[0], results: [{ ...complete().suites[0].specs[0].tests[0].results[0], attachments: ["portable-browser-evidence", "portable-browser-screenshot", "trace"].map(name => ({ name, path: `/tmp/case-${index}/${name}` })) }] }] }] }));
    // A superficially successful result containing only old cases must not pass.
    report.suites = report.suites.filter(suite => suite.title === "portable-core.spec.ts");
    const { validateBrowserReport } = await import("../evals/browser-report.mjs");
    assert.throws(() => validateBrowserReport(report, { count, browser: "chromium", expectedPaths: paths }));
  }
});
test("portable report requires exact executed case count and zero skipped or flaky cases", async () => {
  const { validateBrowserReport } = await import("../evals/browser-report.mjs");
  assert.equal(validateBrowserReport(complete(), { count: 1, browser: "chromium" }).length, 1);
  for (const stats of [{ expected: 0 }, { skipped: 1 }, { flaky: 1 }, { unexpected: 1 }]) assert.throws(() => validateBrowserReport({ ...complete(), stats: { ...complete().stats, ...stats } }, { count: 1, browser: "chromium" }));
});
test("portable evidence rejects duplicate scenarios, reused files and unexpected scenario substitutions", async () => {
  const { validateBrowserReport } = await import("../evals/browser-report.mjs");
  const report = complete(); report.stats.expected = 2;
  report.suites[0].specs.push(structuredClone(report.suites[0].specs[0]));
  assert.throws(() => validateBrowserReport(report, { count: 2, browser: "chromium" }));
  report.suites[0].specs[1].id = "different-id";
  report.suites[0].specs[1].title = "different case";
  assert.throws(() => validateBrowserReport(report, { count: 2, browser: "chromium" }));
  assert.throws(() => validateBrowserReport(complete(), { count: 1, browser: "chromium", expectedPaths: [["portable-core.spec.ts", "another case"]] }));
});
test("portable report rejects missing traces, browser mismatches and error reports", async () => {
  const { validateBrowserReport } = await import("../evals/browser-report.mjs");
  assert.throws(() => validateBrowserReport(complete(), { count: 1, browser: "webkit" }));
  assert.throws(() => validateBrowserReport({ ...complete(), errors: [{ message: "browser crashed" }] }, { count: 1, browser: "chromium" }));
  const report = complete(); report.suites[0].specs[0].tests[0].results[0].attachments.pop();
  assert.throws(() => validateBrowserReport(report, { count: 1, browser: "chromium" }));
});
test("swapping evidence between viewports fails even when titles and revision match", async () => {
  const { validateScenarioIdentity } = await import("../evals/browser-report.mjs");
  const result = { scenarioTitle: "profile", scenarioPath: ["portable-core.spec.ts", "760x520 browser-only core", "profile"] };
  assert.doesNotThrow(() => validateScenarioIdentity({ scenario: "profile", scenarioPath: result.scenarioPath }, result));
  assert.throws(() => validateScenarioIdentity({ scenario: "profile", scenarioPath: ["portable-core.spec.ts", "1040x720 browser-only core", "profile"] }, result));
});
