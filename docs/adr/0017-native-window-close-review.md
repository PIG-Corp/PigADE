# ADR0017 — review dirty documents before native window close

Status: accepted for 0.0.1-dev. Scope: ordinary UI window close.

The former close callback waited only for canvas offsets and otherwise removed the window, losing open context, prompt and profile edits. A second direct exit shortcut also bypassed that callback. BridgeMind's quit-coordinator contract distinguishes dirty documents, submitted writes and independent CLI lifetime.

Keep a separate modal close state, without replacing ProductUi.dialog or its editor buffers. Inventory active documents plus suspended prompt/profile origins and bounded remembered Git drafts. Compare current fields to the saved revision, including edits made after Save was submitted. Navigation/search strings are not documents. The collector exposes opaque keys and labels, never document contents; edit revisions prevent consent for an earlier draft from covering newly changed fields.

Clean, settled UI may close immediately. Dirty documents require explicit discard, with Return selected initially; Escape returns to the existing editor. Submitted operations drain through their existing ACK paths. Their errors are recorded by the coordinator rather than inferred from cleared busy flags. A five-second deadline blocks automatic exit and retains the list of unconfirmed resources. The user may return or explicitly close acknowledging uncertainty; no command is retried. Late failures do not claim data was saved. Native close repetitions cannot bypass review.

The close overlay blocks underlying product, pointer, text/IME and paste mutation routes. A pre-existing IME composition must finish in its original target before the review opens. Mouse placement/resize previews cancel, while an uncommitted keyboard resize is reviewed as a draft and retained on Return; unsent deferred product actions are cancelled rather than launched by quit. Focus/placement and already dirty canvas state use their normal persistence paths. Terminal admission receipts are awaited separately; they do not prove child consumption.

Window removal sends no Stop or launch command. The independently running host owns PTYs. This decision does not implement durable autosave, forced-termination recovery, OS logout inhibition, platform-specific application-wide quit, or guarantees about a host that has already failed. Those remain distinct acceptance work.

Validation uses CloseFlow and collector tests, an exact submitted-Git-draft cleanup regression, and the existing isolated X11 context fixture. The latter must prove close cancellation preserves the edited text and explicit discard exits only the UI with unchanged saved context, layout, generation, process identity, cells and journal. Native Wayland and broader recovery certification remain open.
