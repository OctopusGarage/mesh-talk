import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

export const nativeInvalidCredentialTexts = () => Promise.all(["en", "es", "ja", "zh-Hans", "zh-Hant", "yue"].map(async language => JSON.parse(await readFile(new URL(`../../frontend/src/locales/${language}.json`, import.meta.url), "utf8")).login.invalidCredentials));

export function nativeDriverEvidence(lock) {
  const crate = "tauri-plugin-wdio-webdriver";
  const packages = lock.split(/^\[\[package\]\]\s*$/m).slice(1);
  const matches = packages.filter(block => /^name = "tauri-plugin-wdio-webdriver"$/m.test(block));
  assert.equal(matches.length, 1, "Cargo.lock must contain exactly one native driver package");
  const field = name => {
    const value = matches[0].match(new RegExp(`^${name} = "([^"]+)"$`, "m"))?.[1];
    assert.ok(value, `native driver ${name} missing from Cargo.lock`);
    return value;
  };
  return { crate, version: field("version"), source: field("source"), checksum: field("checksum"), evidence: "Cargo.lock" };
}

// Verify the receipt footer itself is on screen: presence in the virtual list's
// DOM alone does not establish what the native PNG shows.
const receiptObservation = `
const matches=Array.from(document.querySelectorAll('[data-testid=message-bubble]')).filter(e=>e.textContent.includes(arguments[0]));
const e=matches.length===1?matches[0]:null, badges=e?.querySelectorAll('[data-delivery]'), badge=badges?.length===1?badges[0]:null;
const r=e?.getBoundingClientRect(), b=badge?.getBoundingClientRect(), l=document.querySelector('[role=log]')?.getBoundingClientRect();
const visible=!!r&&!!b&&!!l&&r.width>0&&r.height>0&&b.width>0&&b.height>0&&b.top>=Math.max(0,l.top)&&b.bottom<=Math.min(innerHeight,l.bottom)&&b.left>=Math.max(0,l.left)&&b.right<=Math.min(innerWidth,l.right);
return {label:arguments[0],status:badge?.getAttribute('data-delivery'),visible,rect:r?{x:r.x,y:r.y,width:r.width,height:r.height}:null,badgeRect:b?{x:b.x,y:b.y,width:b.width,height:b.height}:null};`;

const authErrorObservation = `
const e=document.querySelector('[data-testid=login-form] [role=alert]'),r=e?.getBoundingClientRect(),form=document.querySelector('[data-testid=login-form]'),f=form?.getBoundingClientRect(),style=e&&getComputedStyle(e);
const visible=!!r&&!!f&&r.width>0&&r.height>0&&f.width>0&&f.height>0&&r.top>=0&&r.left>=0&&r.bottom<=innerHeight&&r.right<=innerWidth&&style.visibility==='visible'&&style.display!=='none'&&Number(style.opacity)>0;
return {text:e?.textContent?.trim(),visible,chatAbsent:!document.querySelector('[data-testid=chat-shell]'),rect:r?{x:r.x,y:r.y,width:r.width,height:r.height}:null};`;

const captureSurface = `
const rect=e=>{const r=e?.getBoundingClientRect();return r?{x:r.x,y:r.y,width:r.width,height:r.height}:null;};
// An entering opacity transition can paint next frame even while opacity is zero.
const onScreen=e=>{const r=e?.getBoundingClientRect(),s=e&&getComputedStyle(e);return !!r&&r.width>0&&r.height>0&&r.bottom>0&&r.top<innerHeight&&r.right>0&&r.left<innerWidth&&s.display!=='none'&&s.visibility==='visible';};
const visible=e=>onScreen(e)&&Number(getComputedStyle(e).opacity)>0;
const describe=e=>({tag:e?.tagName??null,testId:e?.getAttribute?.('data-testid')??null,classes:e?.getAttribute?.('class')??null,rect:rect(e)});
const loaderBlockers=Array.from(document.querySelectorAll('main .animate-spin')).filter(visible).map(describe);
const foregroundLoader=loaderBlockers.length>0;
const animationBlockers=document.getAnimations().filter(a=>a.playState==='running'&&Number.isFinite(a.effect?.getComputedTiming().endTime??Infinity)&&onScreen(a.effect?.target)).map(a=>{const t=a.effect?.getComputedTiming();return {...describe(a.effect?.target),name:a.animationName??null,playState:a.playState,duration:t?.duration,endTime:t?.endTime,iterations:t?.iterations};});
const finiteAnimations=animationBlockers.length;
const dialogs=Array.from(document.querySelectorAll('[role=dialog],[role=alertdialog]')).filter(visible).map(e=>({testId:e.getAttribute('data-testid'),state:e.getAttribute('data-state'),opacity:getComputedStyle(e).opacity,rect:rect(e)}));
return {settled:!foregroundLoader&&finiteAnimations===0,foregroundLoader,finiteAnimations,loaderBlockers,animationBlockers,dialogs,main:rect(document.querySelector('main'))};`;

export async function captureNativeEvidence({ command, execute, until }, expected) {
  let diagnostics;
  try {
    return await until("native evidence visibly painted and stable", async () => {
      await command("POST", "/execute/async", { script: "const done=arguments[arguments.length-1]; requestAnimationFrame(()=>requestAnimationFrame(()=>done(true)));", args: [] });
      const auth = expected?.kind === "auth-error";
      const sample = async () => expected ? execute(auth ? authErrorObservation : receiptObservation, auth ? [] : [expected.label]) : undefined;
      const surfaceBefore = await execute(captureSurface);
      diagnostics = { phase: "surface-before", surfaceBefore };
      if (!surfaceBefore.settled) return false;
      const before = await sample();
      const valid = value => !expected || (value?.visible === true && (auth ? value.chatAbsent === true && expected.texts.includes(value.text) : value.status === expected.status));
      diagnostics = { phase: "expected-before", surfaceBefore, before };
      if (!valid(before)) return false;
      const png = Buffer.from(await command("GET", "/screenshot"), "base64");
      const after = await sample();
      const surfaceAfter = await execute(captureSurface);
      diagnostics = { phase: "surface-after", surfaceBefore, surfaceAfter, before, after };
      if (!surfaceAfter.settled || JSON.stringify(surfaceBefore) !== JSON.stringify(surfaceAfter) || !valid(after) || JSON.stringify(before) !== JSON.stringify(after)) return false;
      return { png, observation: after, surface: surfaceAfter };
    });
  } catch (error) {
    error.captureDiagnostics = diagnostics;
    throw error;
  }
}

/** Diagnostic-only PNG: never used to satisfy a successful scenario. */
export async function captureNativeFailure({ command, execute }) {
  const surface = await execute(captureSurface);
  const png = Buffer.from(await command("GET", "/screenshot"), "base64");
  return { png, surface };
}
