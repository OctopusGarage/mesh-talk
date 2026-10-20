export const REQUIRED_SCENARIOS = ["hide-cancel", "hide", "restart", "settings-search", "offline-restore", "hidden-inbound", "history-retained", "raw-peer-retained", "local-user-isolation", "narrow-keyboard-layout", "privacy-mode", "privacy-reply", "privacy-restart", "auth-register-signin", "auth-logout-wrong-password", "signed-peer-discovery", "direct-messages", "message-content", "group-message", "incoming-attachment", "history-process-restart", "settings-theme-persistence", "native-profile-layout", "native-settings-layout"];
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { nativeInvalidCredentialTexts } from "./native-capture.mjs";
import { inspectPng } from "../evals/png-evidence.mjs";
REQUIRED_SCENARIOS.push("receipt-offline-awaiting", "receipt-peer-restart-delivered", "receipt-source-restart-durable");

const REQUIRED_TRUE = {
  "hide-cancel": ["retained", "cancelDefaultFocus", "enterCancelled"],
  hide: ["absent"], restart: ["actualProcessRestart", "hiddenPersisted", "inboundDurable"],
  "settings-search": ["actualPeerNameFound", "onlineRestoreWorked", "searchAddTabHideWorked"],
  "offline-restore": ["realPeerStopped", "rawRosterExpired", "persistedIdentityRestored", "durableHistoryRetained"],
  "hidden-inbound": ["inboundStored", "rowAbsent"], "history-retained": ["originalEntriesUnchanged", "inboundAdded"],
  "raw-peer-retained": ["rawPeerPresent", "accountPresent"], "local-user-isolation": ["secondUserContactVisible", "secondUserHasNoHiddenEntry"],
  "narrow-keyboard-layout": ["keyboardFocusReachedRestore"], "privacy-mode": ["cancelledBeforeCommit", "passivePeerRetained", "confirmationDismissed", "dialogSettled"],
  "privacy-reply": ["manualGrantAfterRevocation", "realCliReplyStored"], "privacy-restart": ["realCliReplyAfterRestart", "historyRetained"],
  "auth-register-signin": ["registered", "signedIn"], "auth-logout-wrong-password": ["loggedOut", "wrongPasswordRejected", "signinRestored"],
  "signed-peer-discovery": ["cliSawGui"], "direct-messages": ["uiToCliRendered", "uiToCliPersisted", "cliToUiRendered", "cliToUiPersisted"],
  "message-content": ["unicodeRendered", "multilineRendered", "longRendered"], "group-message": ["rendered", "persisted"],
  "incoming-attachment": ["manifestPersisted", "genericFileRendered"], "history-process-restart": ["actualProcessRestart", "rendered", "persisted"],
  "settings-theme-persistence": ["visibleSelection", "persistedAfterRestart"],
  "native-profile-layout": ["profileDialogVisible", "profileUsernameVisible"], "native-settings-layout": ["dialogContained", "themePickerVisible"],
};
const invalidCredentialTexts = await nativeInvalidCredentialTexts();
const evidenceFiles = name => [`${name}.json`, ...(name.startsWith("receipt-") ? [`${name}-0.png`, `${name}-1.png`] : []), `${name}.png`, ...(name === "auth-logout-wrong-password" ? [`${name}-error.png`] : []), `${name}.log`];

export function validateObservations(name, observations, platform) {
  const errors = [];
  const require = (condition, message) => { if (!condition) errors.push(`${name}: ${message}`); };
  if (name.startsWith("receipt-")) {
    require(/^[0-9a-f-]{36}$/.test(observations?.owner ?? ""), "missing owner");
    require(/^[0-9a-f]{32}$/.test(observations?.account ?? ""), "missing target account");
    const ids = observations?.ids;
    require(Array.isArray(ids) && ids.length === 3 && new Set(ids).size === 3 && ids.every(id => /^[0-9a-f]{64}$/.test(id)), "invalid text/sticker/file IDs");
    require(JSON.stringify(ids) === JSON.stringify(observations?.originalIds), "message IDs changed");
    const expected = name === "receipt-offline-awaiting" ? "awaiting" : "delivered";
    require(Array.isArray(observations?.statuses) && observations.statuses.length === 3 && observations.statuses.every(status => status === expected), "incorrect receipt state");
    require(observations?.cardsVerified === true, "missing native card projection");
    const captures = observations?.captures;
    const pngs = evidenceFiles(name).filter(file => file.endsWith(".png"));
    require(Array.isArray(captures) && captures.length === 3 && new Set(captures.map(capture => capture?.label)).size === 3 && captures.every((capture, index) => capture?.filename === pngs[index] && typeof capture.label === "string" && capture.label.length > 0 && capture.visible === true && capture.status === expected && [capture.rect?.x, capture.rect?.y, capture.rect?.width, capture.rect?.height].every(Number.isFinite) && capture.rect.width > 0 && capture.rect.height > 0), "missing painted native card captures");
    require(observations?.actualPeerExit === true, "peer did not actually exit");
    if (name !== "receipt-offline-awaiting") require(observations?.sameKeystorePeerRestart === true, "missing same-keystore restart");
    if (name === "receipt-source-restart-durable") {
      require(observations?.actualProcessRestart === true, "missing app restart");
      require(observations?.peerOfflineDuringSourceRestart === true, "receiver must remain exited throughout source restart");
      require(observations?.offlineBackendDurabilityVerified === true, "missing offline backend checkpoint");
      require(JSON.stringify(observations?.coldIds) === JSON.stringify(ids), "cold checkpoint IDs changed");
      require(Array.isArray(observations?.coldStatuses) && observations.coldStatuses.length === 3 && observations.coldStatuses.every(status => status === "delivered"), "missing cold Delivered projection");
      require(observations?.uiVerifiedAfterPeerRestart === true, "native cards checked after receiver rediscovery");
    }
  }
  if (name === "auth-logout-wrong-password") {
    const capture = observations?.errorCapture;
    require(observations?.sessionReady?.backendReady === true && observations?.sessionReady?.shellReady === true && /^[0-9a-f-]{36}$/.test(observations?.sessionReady?.owner ?? ""), "missing restored ready session capture");
    require(observations?.invalidCredentialsKey === "login.invalidCredentials" && capture?.filename === `${name}-error.png` && invalidCredentialTexts.includes(capture?.text) && capture?.visible === true && capture?.chatAbsent === true && [capture.rect?.x, capture.rect?.y, capture.rect?.width, capture.rect?.height].every(Number.isFinite) && capture.rect.x >= 0 && capture.rect.y >= 0 && capture.rect.width > 0 && capture.rect.height > 0, "missing visible localized credential-error capture");
  }
  if (name === "incoming-attachment") {
    require(observations?.bytesVerified === true, "missing exact decrypted bytes");
    require(observations?.completion?.backendReady === true && observations?.completion?.renderedComplete === true, "missing completed native attachment rendering");
  }
  for (const key of REQUIRED_TRUE[name] ?? []) require(observations?.[key] === true, `missing successful observation ${key}`);
  const nonempty = value => typeof value === "string" && value.trim().length > 0;
  if (name === "auth-register-signin") require(nonempty(observations?.owner), "missing registered account");
  if (name === "signed-peer-discovery") for (const key of ["accountId", "peerUserId"]) require(/^[0-9a-f]{32}$/.test(observations?.[key] ?? ""), `invalid ${key}`);
  if (name === "message-content") require(observations?.persistedCount === 3, "not all content classes persisted");
  if (name === "group-message") require(nonempty(observations?.channelId), "missing real channel");
  if (name === "incoming-attachment") {
    require(nonempty(observations?.fileName), "missing attachment identity");
    require(observations?.downloadVerified === false && nonempty(observations?.limitation), "UI-only attachment coverage must disclose unverified OS download");
  }
  if (name === "settings-theme-persistence") require(observations?.selected === "light", "unexpected persisted theme");
  if (name === "local-user-isolation") require(observations?.secondUserPrivacy?.invisible === false && Array.isArray(observations?.secondUserPrivacy?.allowed_accounts) && observations.secondUserPrivacy.allowed_accounts.length === 0, "privacy leaked between local users");
  const privacyKey = { "privacy-mode": "durable", "privacy-reply": "policy", "privacy-restart": "restored" }[name];
  if (privacyKey) require(observations?.[privacyKey]?.invisible === true, "invisible-mode state not persisted");
  if (name === "native-profile-layout" || name === "native-settings-layout") {
    for (const size of ["defaultLayout", "minimumLayout"]) {
      const layout = observations?.[size], rect = layout?.[name === "native-profile-layout" ? "avatar" : "dialog"], viewport = layout?.viewport;
      require([rect?.x, rect?.y, rect?.right, rect?.bottom, viewport?.width, viewport?.height].every(Number.isFinite) && rect.x >= 0 && rect.y >= 0 && rect.right > rect.x && rect.bottom > rect.y && rect.right <= viewport.width + 1 && rect.bottom <= viewport.height + 1, `invalid ${size} geometry`);
      if (name === "native-profile-layout") {
        require(layout?.topEdgeHit === true, `${size} avatar is not interactive`);
        require(platform === "darwin" ? layout?.chromeClear === true && layout?.chrome?.buttons?.length === 3 : layout?.chromeClear === null && layout?.chrome === null, `${size} inaccurate native chrome coverage`);
      } else require(layout?.dialogContained === true && layout?.themePickerVisible === true, `${size} settings are not visible`);
    }
  }
  return errors;
}

export function validateReceiptSequence(phases) {
  if (phases.length !== 3 || phases.some(phase => !phase)) return ["Missing receipt phase evidence"];
  const identity = phase => JSON.stringify([phase.owner, phase.account, phase.ids]);
  return phases.every(phase => identity(phase) === identity(phases[0])) ? [] : ["Receipt phase owner/account/original IDs differ"];
}

export async function validateEvidence(report, root, expected = {}) {
  const errors = validateReport(report, expected);
  const receipts = new Map();
  for (const name of REQUIRED_SCENARIOS) {
    try {
      const evidence = JSON.parse(await readFile(join(root, `${name}.json`), "utf8"));
      if (evidence.name !== name || evidence.sourceSha !== report.sourceSha || !evidence.observations || typeof evidence.observations !== "object" || Array.isArray(evidence.observations)) throw new Error("invalid or stale observation JSON");
      const observationErrors = validateObservations(name, evidence.observations, report.platform);
      if (observationErrors.length) throw new Error(observationErrors.join("; "));
      if (name.startsWith("receipt-")) receipts.set(name, evidence.observations);
      for (const file of evidenceFiles(name)) {
        const bytes = await readFile(join(root, file));
        if (file.endsWith(".png")) inspectPng(bytes);
        const digest = createHash("sha256").update(bytes).digest("hex");
        if (digest !== report.scenarios?.[name]?.evidenceDigests?.[file]) throw new Error(`Evidence digest mismatch: ${file}`);
      }
    } catch (error) {
      errors.push(`Invalid evidence for ${name}: ${error.message}`);
    }
  }
  errors.push(...validateReceiptSequence(["receipt-offline-awaiting", "receipt-peer-restart-delivered", "receipt-source-restart-durable"].map(name => receipts.get(name))));
  return errors;
}
export function validateReport(report, expected = {}) {
  const errors = [];
  if (report?.failure || report?.cleanupFailure) errors.push("Native evaluation or process cleanup failed");
  if (report?.schema !== 2 || report?.native !== true || report?.mocked !== false || !["darwin", "linux", "win32"].includes(report?.platform)) errors.push("Expected a versioned native desktop report without mocks");
  if (!/^[a-f0-9]{40}$/.test(report?.sourceSha ?? "") || (expected.sourceSha !== undefined && expected.sourceSha !== report?.sourceSha)) errors.push("Missing or mismatched source revision");
  if (expected.platform !== undefined && expected.platform !== report?.platform) errors.push("Mismatched native platform");
  for (const name of REQUIRED_SCENARIOS) {
    const result = report?.scenarios?.[name];
    const files = evidenceFiles(name);
    if (result?.passed !== true || !Number.isFinite(result.elapsedMs) || result.elapsedMs < 0 || !Array.isArray(result.evidence) || result.evidence.length !== files.length || files.some((file, index) => result.evidence[index] !== file || !/^[a-f0-9]{64}$/.test(result.evidenceDigests?.[file] ?? ""))) errors.push(`Missing successful evidence: ${name}`);
  }
  return errors;
}
