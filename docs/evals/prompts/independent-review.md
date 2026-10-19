# Independent AI-Assisted Review

Run this review from a fresh checkout so your AI agent can inspect the source
behind its conclusions. The result is an AI-assisted source review, not a
penetration test or security certification. Keep the review date, commit SHA,
and citations if you share the report.

## How to use

1. Clone the repository: `git clone https://github.com/OctopusGarage/mesh-talk.git`.
2. Enter it: `cd mesh-talk`.
3. Start an AI agent in that directory, such as Codex, Claude Code, or Gemini CLI.
4. Copy the entire prompt below, paste it into the agent, and press Enter.

## Copyable prompt

```markdown
Conduct an independent, evidence-led review of Mesh-Talk for prospective users and contributors. The repository is the current working directory; its public URL is https://github.com/OctopusGarage/mesh-talk.

Act as a software security reviewer, product analyst, and adviser to everyday users. Your job is to determine what the available evidence supports, where it falls short, and whether you would recommend installing the software. A favorable verdict is not required. This is a source review, not a penetration test or security certification.

Review method

1. Establish the exact scope first: review date, `git rev-parse HEAD`, working-tree status, and any release version you actually inspected. Do not assume the current checkout matches a published binary. Do not modify tracked files. If you run a check, record its command, result, and platform.
2. Inspect the README, SECURITY.md, architecture and privacy guides, relevant Rust and frontend code, dependency manifests, tests, CI, release workflow, and download-verification instructions. Use public issues, discussions, advisories, and official documentation for comparison products when online access is available. Prefer implementation evidence to project claims, but remember that code inspection and passing tests do not prove the absence of vulnerabilities.
3. Give every consequential claim a source. For repository claims, link to the reviewed commit and line numbers, or give local paths and line numbers if links are unavailable. For external claims, link to the original source and give its access date. Distinguish **observed** (you ran a check or used the product), **implemented** (you traced a code path but did not run it), **documented** (the project claims it), **inferred** (your reasoning), and **unknown** (insufficient evidence). Say explicitly what you did not inspect or test. Never invent citations, test results, or a hands-on user experience.
4. Use a concrete threat model: a network observer, an unverified first-contact peer, an offline relay, a compromised or lost device, and a malicious or compromised distributor. Examine identity verification and key changes; message and metadata exposure; storage, backups, and plaintext exports; group and multi-device boundaries; permissions; dependencies; update checks; release provenance; and platform or network limitations. Explain which threats each control addresses and which remain.
5. Test the project's strongest claims against contrary evidence. In particular, distinguish no central account from no external network traffic, encrypted content from hidden metadata, a signed release from a reproducible build, and a documented feature from one you personally used. Do not turn hypothetical risks into confirmed vulnerabilities.
6. If the checkout is accessible but the internet is not, complete the local-source review and mark current releases, public reports, and external comparisons as unverified. If you cannot inspect either the checkout or the online repository, stop and request the source; do not produce a simulated review.
7. Handle a suspected exploitable vulnerability through the private process in SECURITY.md. In a public report, describe its impact and evidence limits without publishing actionable exploit steps.

Write a clear, shareable report in English. Lead with the decision, then the evidence. Aim for 1,500–2,000 words excluding citations; use concise tables where they help. Avoid promotional language, blanket reassurance, and repetitive caveats. Use these sections:

# 1. Executive assessment

In at most 120 words, state what Mesh-Talk is, whom it serves, your conditional install recommendation, the strongest reason for it, and the most important unresolved risk. Add a scorecard for security, privacy, usability, and innovation. Score each from 0–10 only when evidence is sufficient; otherwise write "not scored." Explain each score briefly and give confidence (high, medium, or low). Scores are reviewer judgments, not certifications.

# 2. Review scope and evidence

Record the checkout SHA and status, release version if examined, review date, sources inspected, commands actually run, and important gaps. Include a compact table with columns: claim, evidence category, source, and confidence. Keep source-code findings separate from claims about shipped binaries or real-world behavior.

# 3. What the product does

Explain the use case, likely users, required network conditions, and verified features: direct and group messaging, files, offline delivery, linked devices, privacy controls, backup, and recovery. Explain the practical difference from conventional messengers without claiming that every feature works unless the evidence supports it.

# 4. Security assessment

Assess identity establishment and man-in-the-middle exposure, transport and end-to-end encryption, keys and forward secrecy, local storage, group and device boundaries, relay trust, endpoint compromise, dependencies, and release verification. For each important control, describe the evidence, protection, and limit. Distinguish source-code properties from what can be concluded about a downloaded installer.

# 5. Privacy assessment

Explain who could see message content, metadata, contact information, and local exports. Investigate developer access, central services, telemetry, advertising, and profiling without assuming their presence or absence. Compare the trust model with WeChat, Telegram, and Signal using current official sources and access dates; omit or qualify any comparison you cannot verify.

# 6. User experience

Describe installation and first use, learning curve, platform support, reliability evidence, and likely friction. If you have not used the desktop app, label UX observations from screenshots or code as tentative. Separate a user-facing limitation from a security risk.

# 7. Architecture and maintenance

Evaluate the Rust/Tauri structure, peer discovery, synchronization, offline delivery, error handling, tests, CI, release process, and maintenance outlook. Identify concrete strengths and debts with evidence, not adjectives alone.

# 8. Prioritized findings

List the most important risks or gaps in a table: severity, evidence, affected conditions, likely impact, existing mitigation, and recommended action. Explain uncertainty and avoid treating an untested path as a confirmed flaw.

# 9. Questions prospective users ask

Answer directly: "Is it safe to install?" "Can the developers read my chats?" "What happens if I lose a device or the project stops being maintained?" "Why might I choose this over my usual messenger?" "Should a nontechnical person try it now?" Give practical steps such as verifying the download and contact identity where the evidence warrants them.

# 10. Final recommendation for everyday users

In no more than 150 words, explain who should try it, who should wait, what can be trusted on the available evidence, and what remains uncertain. You may recommend against installation. Never call the app "completely safe," claim the developers can see "no information," or describe this report as an AI security certification.
```
