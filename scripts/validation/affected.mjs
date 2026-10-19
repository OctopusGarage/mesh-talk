/** Pure, explainable development selection. This does not certify PR/release readiness. */
import { affectedFrontendTests, isFrontendTest } from "./frontend-graph.mjs";
export const isFrontendGlobal = file =>
  /^(?:frontend\/(?:[^/]+\.(?:json|[cm]?[jt]s)|\.env(?:\..*)?)|tools\/help\/|docs\/help\/|site\/market\/|Cargo\.toml$)/u.test(file);
const safePath = (value) =>
  typeof value === "string" &&
  value.length > 0 &&
  !value.startsWith("/") &&
  !value.startsWith("-") &&
  !value.split("/").includes("..") &&
  !/[\\\x00-\x1f\x7f]/u.test(value);
const matches = (file, pattern) =>
  pattern.endsWith("/") ? file.startsWith(pattern) : file === pattern;
const documentation = (file) =>
  /^(docs\/.*\.md|README\.md|CONTEXT\.md)$/u.test(file);

const text = value => typeof value === "string" && !!value.trim() && !/[\x00-\x1f\x7f]/u.test(value);
export const localGuide = value => typeof value === "string" &&
  safePath(value) && !/[:?]/u.test(value) && /^[^#]+\.md(?:#[a-z0-9][a-z0-9-]*)?$/u.test(value);
function validateCapability(capability, id) {
  if (!capability || typeof capability !== "object" || Array.isArray(capability) ||
      Object.keys(capability).some(key => !["summary", "guide", "invariants"].includes(key)) ||
      !text(capability.summary) || !localGuide(capability.guide) ||
      !Array.isArray(capability.invariants) || !capability.invariants.length ||
      capability.invariants.some(item => !item || typeof item !== "object" ||
        Object.keys(item).some(key => !["name", "reference"].includes(key)) ||
        !text(item.name) || !localGuide(item.reference)))
    throw Error(`Invalid capability for ${id}`);
}

export function validateContracts(contracts) {
  const ids = new Set();
  for (const contract of contracts) {
    for (const key of Object.keys(contract)) {
      if (
        ![
          "id",
          "paths",
          "excludePaths",
          "rust",
          "frontend",
          "node",
          "consumers",
          "escalation",
          "capability",
        ].includes(key)
      )
        throw Error(`Unknown contract field ${key} for ${contract.id}`);
    }
    if (contract.capability !== undefined) validateCapability(contract.capability, contract.id);
    if (!/^[a-z][a-z0-9-]*$/u.test(contract.id))
      throw Error("Invalid validation owner ID");
    if (ids.has(contract.id))
      throw Error(`Duplicate validation owner: ${contract.id}`);
    ids.add(contract.id);
    if (!Array.isArray(contract.paths) || !contract.paths.every(safePath))
      throw Error(`Invalid paths for ${contract.id}`);
    if (contract.excludePaths && (!Array.isArray(contract.excludePaths) || !contract.excludePaths.every(safePath)))
      throw Error(`Invalid excludePaths for ${contract.id}`);
    for (const key of ["frontend", "node"])
      if (
        contract[key] &&
        (!Array.isArray(contract[key]) || !contract[key].every(safePath))
      )
        throw Error(`Invalid ${key} tests for ${contract.id}`);
    for (const item of contract.rust ?? [])
      if (
        !/^[a-z][a-z0-9-]*$/u.test(item.package) ||
        !item.filters?.length ||
        !item.filters.every(
          (filter) =>
            typeof filter === "string" && /^[A-Za-z0-9_:]+$/u.test(filter),
        )
      )
        throw Error(`Invalid Rust selectors for ${contract.id}`);
    for (const key of ["consumers", "escalation"])
      if (
        contract[key] &&
        (!Array.isArray(contract[key]) ||
          !contract[key].every(
            (value) => typeof value === "string" && value.trim(),
          ))
      )
        throw Error(`Invalid ${key} for ${contract.id}`);
  }
  for (const contract of contracts)
    for (const consumer of contract.consumers ?? [])
      if (!ids.has(consumer))
        throw Error(`Unknown consumer ${consumer} of ${contract.id}`);
}

export function buildAffectedPlan(
  files,
  { contracts, frontend, owners = [], reason = "" },
) {
  validateContracts(contracts);
  if (owners.length && !reason.trim())
    throw Error("Explicit owners require --reason explaining the selection");
  const byId = new Map(contracts.map((contract) => [contract.id, contract]));
  const selected = new Map();
  const frontendReasons = {};
  const node = new Set();
  const rust = new Map();
  const blockers = [];
  const escalation = new Set();
  const acknowledged = [];
  function select(id, why) {
    const contract = byId.get(id);
    if (!contract) throw Error(`Unknown owner: ${id}`);
    if (selected.has(id)) {
      selected.get(id).add(why);
      return;
    }
    selected.set(id, new Set([why]));
    for (const item of contract.rust ?? []) {
      if (!rust.has(item.package)) rust.set(item.package, new Set());
      for (const filter of item.filters) rust.get(item.package).add(filter);
    }
    for (const file of contract.frontend ?? []) {
      if (!frontendReasons[file]) frontendReasons[file] = [];
      frontendReasons[file].push(`contract ${id}`);
    }
    for (const file of contract.node ?? []) node.add(file);
    for (const note of contract.escalation ?? []) escalation.add(note);
    for (const consumer of contract.consumers ?? [])
      select(consumer, `consumer of ${id}`);
  }
  for (const owner of owners) select(owner, `explicit: ${reason.trim()}`);
  for (const file of [...new Set(files)].sort()) {
    if (!safePath(file)) throw Error(`Invalid changed path: ${file}`);
    const matching = contracts.filter((contract) =>
      contract.paths.some((pattern) => matches(file, pattern)) &&
      !(contract.excludePaths ?? []).some(pattern => matches(file, pattern)),
    );
    for (const contract of matching) select(contract.id, `changed ${file}`);
    if (frontend && isFrontendGlobal(file)) {
      const tests = [...frontend.current].filter(isFrontendTest);
      for (const test of tests) {
        const relative = test.slice("frontend/".length);
        if (!frontendReasons[relative]) frontendReasons[relative] = [];
        frontendReasons[relative].push(`shared/global input ${file}`);
      }
      if (!tests.length) blockers.push(`Shared frontend input ${file} has no registered unit tests`);
      escalation.add(`Shared frontend input ${file}: review affected browser/native and variant behavior; unit tests alone do not certify runtime wiring.`);
      continue;
    }
    if (frontend && file.startsWith("frontend/src/")) {
      const affected = affectedFrontendTests(file, frontend);
      for (const test of affected.tests) {
        const relative = test.slice("frontend/".length);
        if (!frontendReasons[relative]) frontendReasons[relative] = [];
        frontendReasons[relative].push(`imports changed ${file}`);
      }
      if (affected.computed.length)
        escalation.add(
          `Computed module discovery in ${affected.computed.join(", ")}: review runtime dependencies and select browser/native evidence when needed.`,
        );
      if (affected.tests.length || matching.length) continue;
      escalation.add(
        `Review browser/native behavior for ${file}; source imports cannot prove runtime wiring.`,
      );
      if (!owners.length) {
        blockers.push(
          `No affected unit tests for ${file}; add a contract or select an owner with --reason.`,
        );
        continue;
      }
    } else if (matching.length || documentation(file)) continue;
    if (owners.length) acknowledged.push(file);
    else
      blockers.push(
        `Unmapped change: ${file}; add a contract or select an owner with --reason.`,
      );
  }
  function hasTests(id, visited = new Set()) {
    if (visited.has(id)) return false;
    visited.add(id);
    const contract = byId.get(id);
    return !!(
      contract.rust?.length ||
      contract.node?.length ||
      contract.frontend?.length ||
      (contract.consumers ?? []).some((consumer) => hasTests(consumer, visited))
    );
  }
  for (const id of selected.keys())
    if (!hasTests(id))
      blockers.push(
        `No executable tests for owner ${id}; add selectors or consumer contracts.`,
      );
  for (const file of Object.keys(frontendReasons))
    if (frontend && !frontend.current.has(`frontend/${file}`))
      blockers.push(`Missing registered frontend test: ${file}`);
  return {
    schema: 1,
    scope: "development feedback; mandatory PR/review/CI gates unchanged",
    files: [...new Set(files)].sort(),
    owners: [...selected]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([id, reasons]) => ({ id, reasons: [...reasons].sort(),
        ...(byId.get(id).capability ? { capability: byId.get(id).capability } : {}) })),
    rust: [...rust]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([packageName, filters]) => ({
        package: packageName,
        filters: [...filters].sort(),
      })),
    frontend: Object.keys(frontendReasons).sort(),
    frontendReasons,
    node: [...node].sort(),
    blockers,
    escalation: [...escalation].sort(),
    acknowledged,
  };
}
