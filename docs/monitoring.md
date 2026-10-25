# App company monitoring

The standalone `/monitoring` app page uses the existing durable recurring-order
worker, spending mandates and delivery outbox. It is separate from the frozen Ask
flow and from the host-owned `/v2/company-checks` workflow.

## Setup and execution

1. Search the official KVK Zoeken API as the user types. The authenticated,
   bounded `/v2/monitoring/search` route is sponsored discovery and does not create
   a buyer purchase. Results preserve the exact eight-digit KVK identity.
2. Choose sources: KVK and OpenSanctions by default, optionally GLEIF. Prepare one
   priced initial lookup containing the available KVK operations and chosen
   compliance sources. An establishment profile needs an exact establishment ID.
3. Approve the initial lookup through the ordinary task approval contract. Read
   its saved baseline, available fields, source evidence and coverage gaps.
4. Select available fields, a daily/weekly/monthly schedule and a connected Gmail
   or Slack destination. The recurring quote includes only the required selected
   operations plus the mandatory KVK identity profile. Approve its displayed price
   and spending mandate to activate the schedule.
5. Each scheduled run stores source results and field comparisons. Changed values
   include before/after evidence. Unchanged runs do not create notifications.
   Reopening saved checks does not buy source data.

The processing version is `company_monitor_v1`. Existing recurring orders retain
their original processor. Owner and workspace scope applies to every app route.

## Coverage

KVK comparisons require the exact returned registration number. Branch data also
requires the frozen establishment number. Missing or shielded fields remain
unknown and preserve the last known value; they are not business deletions.
OpenSanctions results are potential matches requiring human identity review, not
confirmed hits or clearance. GLEIF requires a unique record with the selected KVK
number and registration authority `RA000463`; a name match alone is insufficient.
Unavailable operations and incomplete source coverage remain visible. This does
not claim UBO, insolvency, all registry events or real-time change subscriptions.

## Deployment and evidence

Apply `20261025092530_monitoring_kvk_search_credentials.sql` before deploying the
Gateway. Its private runtime-only function returns a credential only for the
active, verified official search route; browser and service roles cannot call it.
Deploy the Gateway before activating any `company_monitor_v1` order, then deploy
the app. Existing recurring execution and delivery infrastructure must be enabled.

Local verification covers source comparison, exact identity, missing coverage,
selection, quoted approval/recovery, autocomplete and migration privileges. The
isolated recurring integration test uses fixture providers and delivery receipts;
it does not constitute a live KVK change or a real email delivery.

Live acceptance must separately record: actual search and selected company;
approved source lookup and saved evidence; approved recurring order and next run;
worker-triggered source execution; unchanged suppression; and a genuine changed
run with its provider delivery receipt when a monitored value changes. Never
alter the baseline or invent a source change to claim live acceptance.
