# Public discovery and factual accuracy review

Evaluate Mesh-Talk's public discovery and answer accuracy using actual retrieved
evidence. Do not infer that a provider performed search from fluent wording or
the presence of a URL. Never substitute a source-code review for a live answer.

## Inputs and procedure

Record the date, product and exposed model, language/region, search mode, exact
prompt, full answer, cited URLs, retrieval failures and documentation version.
Keep output under ignored `target/geo/` or in PR artifacts. Use fresh sessions
and repeat questions at least three times for an exploratory comparison. Use
only account access already authorized by the user; mark unavailable products
as not run. Separate consumer chat interfaces from API model/tool combinations.

Run three tracks independently:

- Discovery: ask neutral tool-selection questions without naming Mesh-Talk or
  providing its URL. Include unsupported-fit cases; omission is not always an error.
- Branded search: ask the questions below by project name with search enabled.
- Direct retrieval: supply the official website, its Markdown alternate or the
  explicitly configured read-only documentation MCP. Do not count this as discovery.

## Question set and acceptance criteria

| ID     | Question                                                           | Required distinction                                                              |
| ------ | ------------------------------------------------------------------ | --------------------------------------------------------------------------------- |
| GEO-01 | What is Mesh-Talk?                                                 | Encrypted desktop messenger for local and connected networks                      |
| GEO-02 | Do I need an account?                                              | Local account required; cloud identity provider not required                      |
| GEO-03 | Can I chat without internet?                                       | Yes on a reachable LAN; disconnected devices are not instantly reachable          |
| GEO-04 | Can it connect computers on separate networks?                     | Existing routes or configured bridges, not automatic Internet-wide connectivity   |
| GEO-05 | Does it require a server?                                          | No required vendor service; optional peer-operated post offices are running nodes |
| GEO-06 | Is offline delivery guaranteed?                                    | Connectivity, eligibility, capacity and retention constrain delivery              |
| GEO-07 | Can a post office read messages?                                   | No message plaintext; routing metadata may remain visible                         |
| GEO-08 | Does seeing a peer prove my message arrived?                       | Discovery, transport, admission, custody and recipient confirmation differ        |
| GEO-09 | Does a delivered file card prove the file is downloadable?         | Attachment-byte availability is separate from retained event metadata             |
| GEO-10 | Which platforms are supported?                                     | macOS, Windows and Linux desktop; verify published package availability           |
| GEO-11 | Does Mesh-Talk include a language model?                           | Public help invokes no model; external assistants are separate                    |
| GEO-12 | Does the documentation MCP read private chats?                     | It serves public help only; messaging integration has separate opt-in permissions |
| GEO-13 | Is the online documentation version the latest Stable release?     | Source version does not prove Stable publication                                  |
| GEO-14 | How do I read the site as Markdown?                                | Explicit alternate URLs work; HTML Accept negotiation is not promised             |
| GEO-15 | Should I use it as a hosted browser-only global messaging service? | Do not invent unsupported product surfaces or vendor infrastructure               |

Derive final answers from the current public manual and release evidence, not
solely this rubric. Use natural paraphrases; preserve the same task intent across
before/after comparisons. For multilingual evaluation, record reviewed equivalent
questions separately and do not confuse translated navigation with translated help.

## Scoring and limitations

Report eligible-task mention rate, official-source citation rate, supported-claim
accuracy, contradiction rate and version accuracy separately. Check that each
citation supports its adjacent claim. Mark claims without retrievable evidence
as unsupported or unassessable rather than assuming correctness. Preserve
per-question evidence so aggregate scores can be audited.

Compare identical product/mode conditions after deployment and recrawl. A changed
answer is an observation, not proof of causation. Webmaster citation reports,
verified crawl logs and referral analytics are supporting evidence with different
coverage; none alone measures correctness or all zero-click exposure. Static
tests prove publication contracts, not search placement or model recommendations.
