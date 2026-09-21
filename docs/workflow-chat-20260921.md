# Workflows start in Ask — 21 September 2026

## Change

Every workflow row opens the normal Ask conversation. Old form URLs redirect there. The ten recipe workflows send an explicit, allowlisted workflow slug with the fresh chat request. Supplier payment check retains its invoice conversation.

The gateway keeps collected values in the saved task, uses the conversational model to extract user-supplied values and ask for missing required details, and builds the existing source recipe only after validation. Optional fields remain optional. Invalid inputs stay in the chat. An unexecuted quote can be corrected in the same chat and receives a new quote; neither chat text nor workflow selection approves spending. Quote, approval, execution, recovery and source selection use the existing gateway machinery.

## Verification before release

- Gateway full suite: 242 passed, 0 failed, 27 environment-dependent tests ignored.
- Dedicated isolated database acceptance: supplier invoice flow and all ten conversational recipe flows passed. Exercises missing/partial input, saved-chat recovery, replay, invalid country, quote correction, one fixture approval, source execution, result recovery and history. Includes source failures, ambiguous addresses and cancellation. Source, billing and model responses in these tests are fixtures.
- App focused workflow/chat suite: 38 passed.
- App full suite: 2216 passed, four existing failures in standalone manifest and workspace selection tests (unchanged from the preceding release).
- App production build, TypeScript and lint passed.

## Reported production errors

Read-only inspection of recent gateway requests and workflow runs found no saved error receipts or failed new workflow runs at investigation time. The exact user-reported error was requested. This release fixes the confirmed form-versus-chat mismatch; it is not evidence that an unidentified provider error has been fixed.

No paid production request was approved or replayed. Live chat/quote verification will be recorded separately from fixture execution.
