import assert from "node:assert/strict";
import { test } from "node:test";

const complete = () => ({ stats: { expected: 1, unexpected: 0, skipped: 0, flaky: 0 }, errors: [], suites: [{ title: "portable-core.spec.ts", specs: [{ id: "fixture-id", title: "fixture case", ok: true, tests: [{ projectName: "chromium", status: "expected", results: [{ status: "passed", attachments: ["portable-browser-evidence", "portable-browser-screenshot", "trace"].map(name => ({ name, path: `/tmp/evidence/${name}` })) }] }] }] }] });
test("portable matrix requires existing receipt and media cases in both tiers", async () => {
  const { requiredBrowserPaths } = await import("../evals/browser-report.mjs");
  for (const [tier, count] of [["core", 62], ["extended", 63]]) {
    const paths = requiredBrowserPaths(tier);
    assert.equal(paths.length, count);
    assert.ok(paths.some(path => path[0] === "portable-core.spec.ts" && path[1] === "presence waits for node readiness without showing a startup failure"));
    assert.ok(paths.some(path => path[1] === "city avatar thumbnails load after scrolling the gallery"));
    assert.equal(paths.filter(path => path[0] === "automatic-delivery.spec.ts").length, 11);
    assert.ok(paths.some(path => path[0] === "automatic-delivery.spec.ts" && path[1] === "a scoped native delivery invalidation refreshes status without a polling response"));
    for (const result of ["acceptance", "rejection"]) assert.ok(paths.some(path => path[0] === "automatic-delivery.spec.ts" && path[1] === `submitted text stays in its bubble after ${result} and navigation`));
    for (const size of ["1280x800", "760x620", "320x360"]) {
      assert.ok(paths.some(path => path[1] === `delivery details preserve keyboard focus and fit ${size}`));
    }
    assert.ok(paths.some(path => path[1] === "delivery details close on navigation and app lock, including a late status response"));
    assert.ok(paths.some(path => path[0] === "automatic-delivery.spec.ts" && path[1] === "delivery recovery survives its receipt and returns focus safely, then retires on logout"));
    assert.equal(paths.filter(path => path[0] === "media-recovery.spec.ts").length, 1);
    assert.equal(paths.filter(path => path[0] === "media-lifecycle.spec.ts").length, 3);
    assert.equal(paths.filter(path => path[0] === "screenshot-portable.spec.ts").length, 7);
    assert.equal(paths.filter(path => path[0] === "file-send-progress.spec.ts").length, 4);
    assert.equal(paths.filter(path => path[0] === "directory-transfer.spec.ts").length, 3);
    assert.equal(paths.filter(path => path[0] === "attachment-approval.spec.ts").length, 4);
    assert.ok(paths.some(path => path[0] === "attachment-approval.spec.ts" && path[1] === "an expired previously ready attachment becomes unavailable"));
    assert.equal(paths.filter(path => path[0] === "unread-badge.spec.ts").length, 11);
    assert.ok(paths.some(path => path[0] === "unread-badge.spec.ts" && path[1] === "one logical DM remains one unread badge across duplicate live events and history scanning"));
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


test("isolated portable shards preserve the complete strict inventory and every failure", async () => {
  const { portableShards, mergePortableReports } = await import("../evals/portable-runner.mjs");
  const { validateBrowserReport } = await import("../evals/browser-report.mjs");
  assert.equal(portableShards("win32"), 2);
  for (const os of ["darwin", "linux"]) assert.equal(portableShards(os), 1);
  const first = complete(), second = structuredClone(first);
  second.suites[0].specs[0].id = "second";
  second.suites[0].specs[0].title = "second case";
  for (const attachment of second.suites[0].specs[0].tests[0].results[0].attachments) attachment.path += "-second";
  const options = { count: 2, browser: "chromium", expectedPaths: [
    ["portable-core.spec.ts", "fixture case"], ["portable-core.spec.ts", "second case"],
  ] };
  assert.equal(validateBrowserReport(mergePortableReports([first, second]), options).length, 2);
  assert.throws(() => validateBrowserReport(mergePortableReports([first]), options), /Missing or zero/);
  assert.throws(() => validateBrowserReport(mergePortableReports([first, first]), options), /Duplicate/);
  for (const statistic of ["unexpected", "skipped", "flaky"]) {
    const bad = structuredClone(second); bad.stats[statistic] = 1;
    assert.throws(() => validateBrowserReport(mergePortableReports([first, bad]), options), /Nonpassing/);
  }
  const bad = structuredClone(second); bad.errors.push({ message: "worker failed" });
  assert.throws(() => validateBrowserReport(mergePortableReports([first, bad]), options), /harness errors/);
  assert.throws(() => mergePortableReports([]), /Missing/);
});
