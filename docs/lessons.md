# Lessons

- **Test helpers can hand out public keys.** `near-workspaces` gives sub-accounts created without
  explicit keys a key from a fixed public seed. Harmless in a sandbox, dangerous on testnet and in demos
  that say "this key is yours". Always pass `SecretKey::from_random`.
- **Don't strand your own kill switch.** Deleting the agent's full-access key looked like the strongest
  hand-over, but then nobody could fire the agent. Least privilege applies to the key you hand out, not
  to the owner's recovery path.
- **A gate that runs first hides everything after it.** The prebuilt-hash check masked mutation tests.
  Independent properties belong in independent tests.
- **Reproducible wasm needs path remapping on stable Rust.** Order the `--remap-path-prefix` flags so the
  most specific prefix comes last.
- **Public RPCs rate-limit.** Retry with backoff, try several endpoints and let the user override.
- **Renames miss untracked files.** Search the working tree, not only `git ls-files`, before calling a
  rename done.
- **Write the expected output from real runs.** README and VERIFY.md quote actual output, so a reader
  can diff their run against it.
- **Reproducible per host is not reproducible.** A build that matches in every x86_64 container can
  still differ on arm64. Pin the build platform, not only the toolchain, and say what each check
  proves.
- **A checker must compare, not print.** An observer that prints the chain's answer next to a claim
  looks like evidence, but it fails only if it compares. Feed it altered records and require it to
  fail.
- **Measure the number you print.** "You received 0" should come from the receipts that could have
  paid you, with a positive control that the measurement can see a payment.
- **Every stored record is a storage liability.** Bound what an untrusted caller can make you store,
  and reserve its cost up front.
- **The model proposes, the contract disposes.** A prompt-injected agent is expected; the demo sends
  the injected payment regardless of what the model chose, so the guarantee does not depend on the
  model.
- **A lifetime limit is a wedge.** One flood used up the v0.9 mandate's payment slots for good.
  Limits an untrusted caller can use up should reset with the block height and say when.
- **Check the bytes, not only the source.** The exports showed no admin method, but the high-level
  promise API still linked `deploy_contract` and `add_key` into the wasm. Reading the import section
  turns "no admin path" into a property of the deployed code.
- **Measure after the refund.** A function-call key's allowance drops by the prepaid gas first and
  comes back a few blocks later; a reading taken at once is wrong by three orders of magnitude.
