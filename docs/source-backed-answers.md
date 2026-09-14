# Every question is answered, not only shown

## What changed

A finished request used to end at the saved source response: the App drew the
result card and its JSON, and a person read the answer out of the payload
themselves. The gateway only wrote prose when the question itself asked for an
interpretation (`answer_operations`), which the planner deliberately leaves
empty for plain retrievals.

The gateway now writes one answer for every finished question. When the paid
steps of a request are done and no answer exists yet, `advance` queues the
existing `Analyze` action, and the answer model receives the person's question
alongside the saved documents. The App shows it above the result card under the
heading **Answer**, and the chatbot contract asks hosts to lead with it.

## Why it is built this way

- **The answer is a stage, not part of the last call.** It stays a separate
  action so the result a person paid for appears first and the sentence follows,
  rather than the last execution blocking on a model call. `status` is `running`
  and `context_view.analyzing` is true while it is written; both the App poll
  loop and the server worker already drive that action.
- **It buys nothing.** The answer reads documents that were already paid for.
  No proposal, approval or provider call is involved, so it also runs while
  multi-step research is paused (`V2_SINGLE_CALL_MODE`).
- **Grounding is unchanged.** Prose still carries no digits or links: every
  figure is a JSON pointer into a saved document, resolved and rendered by
  trusted code. The App now shows those cited values next to the sentence
  instead of hiding them behind a disclosure, because a hidden number is not an
  answer to "how many" or "which".
- **A missing answer no longer downgrades a complete retrieval.** `partial` is
  reserved for a failed answer to an explicitly requested comparison; a plain
  retrieval whose answer failed stays `succeeded`, shows the reason, and offers
  the free retry action over the same saved documents.

## Costs and limits

- One extra model call per finished request, and the seconds it takes. That is
  the price of answering the question that was asked; it is bounded by the
  existing answer-model timeouts and never repeats itself on a poll.
- Large responses are shortened before the model reads them
  (`src/answer_input.rs`): arrays keep their first rows and strings their first
  characters, so every value the model may cite still resolves at the same JSON
  pointer. A shortened document is marked, and the prompt asks for that caveat
  among the answer's limitations.
- The answer is written from the documents of the current request only. It is
  not a conversation summary and carries no memory of earlier turns.
