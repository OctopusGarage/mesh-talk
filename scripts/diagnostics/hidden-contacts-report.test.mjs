import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { createHash } from "node:crypto";
import { pngFixture } from "./test-fixtures/png.mjs";
import { inspectPng } from "../evals/png-evidence.mjs";
import { join } from "node:path";
import { validateReport, validateEvidence, REQUIRED_SCENARIOS } from "./hidden-contacts-report.mjs";

const paintProof = () => ({ viewport: { width:500,height:500 }, badgeRect: { x:0,y:0,width:100,height:100 }, paint: { coordinateBasis:"webview-client", screenshot:{width:500,height:500}, bounds:{x:0,y:0,width:100,height:100}, horizontalEdges:20, verticalEdges:20, painted:true, pixelDigest:"a".repeat(64), matchingFrames:2 } });
const captures = (name, status) => ["text", "sticker", "file"].map((label, index) => ({ ...paintProof(), label, filename: index === 2 ? `${name}.png` : `${name}-${index}.png`, visible: true, status, rect: { x: 0, y: 0, width: 100, height: 100 } }));
const complete = () => ({
  schema: 2, sourceSha: "a".repeat(40), platform: process.platform, native: true, mocked: false,
  scenarios: Object.fromEntries(REQUIRED_SCENARIOS.map(name => [name, { passed: true, elapsedMs: 1, evidence: [`${name}.json`, ...(name.startsWith("receipt-") ? [`${name}-0.png`, `${name}-1.png`] : []), `${name}.png`, ...(name === "auth-logout-wrong-password" ? [`${name}-error.png`] : []), `${name}.log`], evidenceDigests: Object.fromEntries([`${name}.json`, ...(name.startsWith("receipt-") ? [`${name}-0.png`, `${name}-1.png`] : []), `${name}.png`, ...(name === "auth-logout-wrong-password" ? [`${name}-error.png`] : []), `${name}.log`].map(file => [file, "b".repeat(64)])) }])),
});
test("requires three real receipt phases with exact stable identities", async () => {
  const { validateObservations } = await import("./hidden-contacts-report.mjs");
  for (const name of ["receipt-offline-awaiting", "receipt-peer-restart-delivered", "receipt-source-restart-durable"]) {
    assert.ok(REQUIRED_SCENARIOS.includes(name));
    assert.ok(validateObservations(name, {}).length);
  }
});
test("receipt phases reject changed identity, malformed IDs and false state", async () => {
  const { validateObservations, validateReceiptSequence } = await import("./hidden-contacts-report.mjs");
  const ids = ["a".repeat(64), "b".repeat(64), "c".repeat(64)];
  const observation = { owner: "a".repeat(36), account: "d".repeat(32), ids, originalIds: ids, statuses: ["awaiting", "awaiting", "awaiting"], cardsVerified: true, actualPeerExit: true, captures: captures("receipt-offline-awaiting", "awaiting") };
  assert.deepEqual(validateObservations("receipt-offline-awaiting", observation), []);
  for (const patch of [{ statuses: ["delivered", "awaiting", "awaiting"] }, { actualPeerExit: false }, { ids: ["bad"] }, { originalIds: [...ids].reverse() }]) assert.ok(validateObservations("receipt-offline-awaiting", { ...observation, ...patch }).length);
  const phases = [observation, { ...observation }, { ...observation }];
  assert.deepEqual(validateReceiptSequence(phases), []);
  for (const patch of [{ owner: "b".repeat(36) }, { account: "e".repeat(32) }, { ids: [...ids].reverse() }]) assert.ok(validateReceiptSequence([observation, { ...observation, ...patch }, observation]).length);
  const durable = { ...observation, captures: captures("receipt-source-restart-durable", "delivered"), statuses: ["delivered", "delivered", "delivered"], sameKeystorePeerRestart: true, actualProcessRestart: true };
  assert.ok(validateObservations("receipt-source-restart-durable", durable).length, "online receiver can hide missing persisted delivery");
  assert.ok(validateObservations("receipt-source-restart-durable", { ...durable, peerOfflineDuringSourceRestart: true }).length, "offline backend checkpoint is required before online rendering");
  assert.deepEqual(validateObservations("receipt-source-restart-durable", { ...durable, peerOfflineDuringSourceRestart: true, offlineBackendDurabilityVerified: true, coldIds: ids, coldStatuses: ["delivered", "delivered", "delivered"], uiVerifiedAfterPeerRestart: true }), []);
});
test("requires native invisible-mode, reply and restart evidence", () => {
  for (const name of ["privacy-mode", "privacy-reply", "privacy-restart"]) {
    assert.ok(REQUIRED_SCENARIOS.includes(name), `missing ${name}`);
    const report = complete();
    delete report.scenarios[name];
    assert.ok(validateReport(report).some(error => error.includes(name)));
  }
});
test("rejects absent screenshots, path traversal and malformed timings", () => {
  for (const patch of [{ evidence: ["hide.json"] }, { evidence: ["../hide.json", "hide.png"] }, { elapsedMs: undefined }, { elapsedMs: -1 }]) {
    const report = complete();
    Object.assign(report.scenarios.hide, patch);
    assert.ok(validateReport(report).length);
  }
});
test("requires evidence files to exist and contain valid JSON and PNG data", async () => {
  const root = await mkdtemp(join(tmpdir(), "native-contact-evidence-"));
  const report = complete();
  assert.ok((await validateEvidence(report, root)).length);
  for (const name of REQUIRED_SCENARIOS) {
    await writeFile(join(root, `${name}.json`), "not json");
    await writeFile(join(root, `${name}.png`), "not an image");
  }
  assert.ok((await validateEvidence(report, root)).length);
});
test("accepts a complete native evaluation", () => assert.deepEqual(validateReport(complete()), []));
test("requires expanded core scenarios, exact source revision and evidence hashes", () => {
  for (const name of ["auth-register-signin", "direct-messages", "incoming-attachment", "native-profile-layout", "history-process-restart"]) assert.ok(REQUIRED_SCENARIOS.includes(name));
  for (const sourceSha of [undefined, "main", "0".repeat(39)]) assert.ok(validateReport({ ...complete(), sourceSha }).length);
  assert.ok(validateReport(complete(), { sourceSha: "c".repeat(40) }).length);
  assert.ok(validateReport(complete(), { platform: "invalid" }).length);
  const report = complete(); report.scenarios.hide.evidenceDigests["hide.png"] = "invalid";
  assert.ok(validateReport(report).length);
});
test("typed observations reject missing checks and preserve explicit download limitations", async () => {
  const { validateObservations } = await import("./hidden-contacts-report.mjs");
  assert.deepEqual(validateObservations("direct-messages", { uiToCliRendered: true, uiToCliPersisted: true, cliToUiRendered: true, cliToUiPersisted: true }), []);
  for (const observations of [{}, { uiToCliRendered: "true" }, { uiToCliRendered: false }]) assert.ok(validateObservations("direct-messages", observations).length);
  assert.deepEqual(validateObservations("incoming-attachment", { fileName: "fixture.txt", manifestPersisted: true, genericFileRendered: true, bytesVerified: true, completion: {backendReady:true,renderedComplete:true}, downloadVerified: true, limitation: "OS save picker is not automated" }), []);
  assert.ok(validateObservations("incoming-attachment", { fileName: "fixture.txt", manifestPersisted: true, genericFileRendered: true, downloadVerified: true, limitation: "OS save picker is not automated" }).length);
  assert.ok(validateObservations("incoming-attachment", { manifestPersisted: true, genericFileRendered: true, downloadVerified: true }).length);
});
test("rejects missing, failed and evidence-free scenarios", () => {
  for (const value of [undefined, { passed: false, evidence: ["x"] }, { passed: true, evidence: [] }]) {
    const report = complete();
    report.scenarios[REQUIRED_SCENARIOS[0]] = value;
    assert.ok(validateReport(report).length);
  }
});
test("rejects renderer mocks and unsupported platforms", () => {
  for (const patch of [{ native: false }, { mocked: true }, { platform: "unknown" }, { schema: 0 }, { failure: "driver died" }, { cleanupFailure: "child remained alive" }]) {
    assert.ok(validateReport({ ...complete(), ...patch }).length);
  }
});

test("receipt evidence requires a painted screenshot observation for each card", async () => {
  const { validateObservations } = await import("./hidden-contacts-report.mjs");
  const observations = { owner: "a".repeat(36), account: "b".repeat(32), ids: ["c".repeat(64), "d".repeat(64), "e".repeat(64)], statuses: ["awaiting", "awaiting", "awaiting"], cardsVerified: true, actualPeerExit: true };
  observations.originalIds = observations.ids;
  assert.ok(validateObservations("receipt-offline-awaiting", observations).some(error => error.includes("capture")));
  const valid = { ...observations, captures: captures("receipt-offline-awaiting", "awaiting") };
  assert.deepEqual(validateObservations("receipt-offline-awaiting", valid), []);
  for (const patch of [{ visible: false }, { status: "delivered" }, { filename: "wrong.png" }, { label: "" }, { rect: { width: 0, height: 0 } }]) {
    const changed = { ...valid, captures: valid.captures.map((capture, index) => index === 0 ? { ...capture, ...patch } : capture) };
    assert.ok(validateObservations("receipt-offline-awaiting", changed).length);
  }
});

test("wrong-password proof requires its localized visible error and supplemental PNG", async () => {
  const { validateObservations } = await import("./hidden-contacts-report.mjs");
  const name = "auth-logout-wrong-password";
  const observations = { loggedOut: true, wrongPasswordRejected: true, signinRestored: true };
  assert.ok(validateObservations(name, observations).length, "a recovered chat screenshot alone does not prove the error UI");
  const valid = { ...observations, sessionReady: {owner:"a".repeat(36),backendReady:true,shellReady:true}, invalidCredentialsKey: "login.invalidCredentials", errorCapture: { ...paintProof(), filename: `${name}-error.png`, text: "用户名或密码错误", visible: true, chatAbsent: true, rect: { x: 10, y: 10, width: 100, height: 20 } } };
  assert.deepEqual(validateObservations(name, valid), []);
  for (const patch of [{ visible: false }, { text: "decrypt failed" }, { filename: "wrong.png" }, { chatAbsent: false }, { rect: { x: 0, y: 0, width: 0, height: 0 } }]) assert.ok(validateObservations(name, { ...valid, errorCapture: { ...valid.errorCapture, ...patch } }).length);
  const report = complete();
  report.scenarios[name].evidence = report.scenarios[name].evidence.filter(file => !file.endsWith("-error.png"));
  assert.ok(validateReport(report).some(error => error.includes(name)), "error capture must be a hashed report artifact");
});

test("semantic capture checkpoints cannot pass from backend-only observations", async()=>{
  const {validateObservations}=await import('./hidden-contacts-report.mjs');
  const attachment={fileName:'file',manifestPersisted:true,genericFileRendered:true,bytesVerified:true,recipientApproved:true,downloadVerified:true,limitation:'OS save picker not invoked'};
  assert.ok(validateObservations('incoming-attachment',attachment).length);
  assert.deepEqual(validateObservations('incoming-attachment',{...attachment,completion:{backendReady:true,renderedComplete:true}}),[]);
  const privacy={cancelledBeforeCommit:true,durable:{invisible:true},passivePeerRetained:true};
  assert.ok(validateObservations('privacy-mode',privacy).length);
  assert.deepEqual(validateObservations('privacy-mode',{...privacy,confirmationDismissed:true,dialogSettled:true}),[]);
});

test("capture proof requires viewport coordinates and matching pixel frames", async () => {
 const {validateObservations}=await import("./hidden-contacts-report.mjs");
 const name="auth-logout-wrong-password";
 const valid={loggedOut:true,wrongPasswordRejected:true,signinRestored:true,sessionReady:{owner:"a".repeat(36),backendReady:true,shellReady:true},invalidCredentialsKey:"login.invalidCredentials",errorCapture:{...paintProof(),filename:`${name}-error.png`,text:"用户名或密码错误",visible:true,chatAbsent:true,rect:{x:10,y:10,width:100,height:20}}};
 assert.deepEqual(validateObservations(name,valid),[]);
 for(const patch of [{viewport:undefined},{paint:undefined},{paint:{...valid.errorCapture.paint,matchingFrames:1}},{paint:{...valid.errorCapture.paint,painted:false}},{paint:{...valid.errorCapture.paint,coordinateBasis:"window"}}]) assert.ok(validateObservations(name,{...valid,errorCapture:{...valid.errorCapture,...patch}}).length);
});

test("offline validation recomputes region paint from PNG instead of trusting capture booleans", async () => {
 const root=await mkdtemp(join(tmpdir(),"native-pixel-proof-"));
 const name="receipt-offline-awaiting",report=complete();
 const region={rect:{x:20,y:20,width:20,height:12},viewport:{width:64,height:64}};
 const good=pngFixture({pixel:(x,y)=>x>=20&&x<40&&y>=20&&y<32?[(x%8<3&&y>22&&y<30)?85:247,247,247,255]:[x*3,y*3,x+y,255]});
 const blank=pngFixture({pixel:(x,y)=>x>=20&&x<40&&y>=20&&y<32?[247,247,247,255]:[x*3,y*3,x+y,255]});
 const proof={...inspectPng(good,region).region,matchingFrames:2};
 const ids=["a".repeat(64),"b".repeat(64),"c".repeat(64)];
 const observations={owner:"a".repeat(36),account:"d".repeat(32),ids,originalIds:ids,statuses:["awaiting","awaiting","awaiting"],cardsVerified:true,actualPeerExit:true,captures:captures(name,"awaiting").map(c=>({...c,viewport:region.viewport,badgeRect:region.rect,paint:proof}))};
 const files=new Map([[`${name}.json`,Buffer.from(JSON.stringify({name,sourceSha:report.sourceSha,observations}))],[`${name}.log`,Buffer.from("real peer log")],...observations.captures.map(c=>[c.filename,good])]);
 const write=async()=>{for(const [file,bytes] of files){await writeFile(join(root,file),bytes);report.scenarios[name].evidenceDigests[file]=createHash("sha256").update(bytes).digest("hex");}};
 await write();
 assert.equal((await validateEvidence(report,root)).some(e=>e.startsWith(`Invalid evidence for ${name}:`)),false);
 files.set(observations.captures[0].filename,blank);await write();
 assert.ok((await validateEvidence(report,root)).some(e=>e.startsWith(`Invalid evidence for ${name}:`)&&/paint|pixel/i.test(e)),"even correctly rehashed PNG must contain its expected painted region");
 const authName="auth-logout-wrong-password";
 const errorCapture={filename:`${authName}-error.png`,text:"用户名或密码错误",visible:true,chatAbsent:true,rect:region.rect,viewport:region.viewport,paint:proof};
 const authObservations={loggedOut:true,wrongPasswordRejected:true,signinRestored:true,sessionReady:{owner:"a".repeat(36),backendReady:true,shellReady:true},invalidCredentialsKey:"login.invalidCredentials",errorCapture};
 for(const [file,bytes] of [[`${authName}.json`,Buffer.from(JSON.stringify({name:authName,sourceSha:report.sourceSha,observations:authObservations}))],[`${authName}.png`,good],[errorCapture.filename,good],[`${authName}.log`,Buffer.from("auth fixture log")]]) {
  await writeFile(join(root,file),bytes); report.scenarios[authName].evidenceDigests[file]=createHash("sha256").update(bytes).digest("hex");
 }
 assert.equal((await validateEvidence(report,root)).some(e=>e.startsWith(`Invalid evidence for ${authName}:`)),false);
 await writeFile(join(root,errorCapture.filename),blank);report.scenarios[authName].evidenceDigests[errorCapture.filename]=createHash("sha256").update(blank).digest("hex");
 assert.ok((await validateEvidence(report,root)).some(e=>e.startsWith(`Invalid evidence for ${authName}:`)&&/paint|pixel/i.test(e)),"localized error screenshot also requires its own painted pixel region");
});
