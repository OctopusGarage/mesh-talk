/** Static obligations for a development plan; browser/native proof stays explicit. */
export function buildStaticChecks(plan) {
  const checks = [];
  const add = (name, command, cwd = ".", inputs) => checks.push({ name, command, cwd, ...(inputs ? { inputs } : {}) });
  const packages = [...new Set(plan.rust.map(item => item.package))].sort();
  if (plan.files.some(file => file.startsWith(".github/workflows/")))
    add("workflow-lint", ["actionlint"]);
  const shells = plan.files.filter(file => !plan.deleted?.includes(file) && /^(?:scripts\/.*\.sh|hooks\/[^/]+)$/u.test(file));
  if (shells.length) add("shell-lint", ["shellcheck", "--severity=warning", "--", ...shells]);
  if (plan.files.some(file => ["scripts/build-retention.py", "scripts/test_build_retention.py"].includes(file)))
    add("build-retention-tests", ["python3", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_build_retention.py", "-v"]);
  if (packages.length) add("rust-format", ["cargo", "fmt", "--all", "--", "--check"]);
  const frontend = plan.frontend.length > 0 || plan.files.some(file =>
    /^(frontend\/|tools\/help\/|docs\/help\/|site\/market\/)/u.test(file));
  if (frontend) {
    add("frontend-format", ["npx", "prettier", "--check", "src/"], "frontend", "frontend");
    add("frontend-lint", ["npm", "run", "lint"], "frontend", "frontend");
    add("frontend-types", ["npm", "run", "typecheck"], "frontend", "frontend");
  }
  // generate_context! reads frontendDist even for lint/test compilation.
  if (frontend || packages.includes("mesh-talk")) {
    add("frontend-build", ["npm", "run", "build"], "frontend", "frontend");
    checks.at(-1).artifacts = ["dist"];
    checks.at(-1).resource = "heavy";
  }
  if (packages.length) {
    add("rust-clippy", ["cargo", "clippy", "--locked",
      ...packages.flatMap(name => ["-p", name]), "--all-targets", "--", "-D", "warnings"]);
    checks.at(-1).resource = "heavy";
  }
  return checks;
}

export function phasePlan(plan, phase) {
  if (!["static", "tests"].includes(phase)) throw Error(`Unknown validation phase: ${phase}`);
  const checks = (plan.checks ?? []).filter(check => (check.resource === "heavy") === (phase === "tests"));
  return phase === "static" ? { ...plan, checks, rust: [], node: [], frontend: [] } : { ...plan, checks };
}
