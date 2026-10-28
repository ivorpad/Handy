# Voice workspace

Open **Workspace** in Handy's sidebar. This guide covers macOS.

| Section               | Use                                                                                                                                                                                                            |
| --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Snippets & vocabulary | Add dictionary words and the spellings a model writes instead. Save spoken triggers with exact plain text. Review corrections from history, or enable local background spelling learning with review and undo. |
| Writing profiles      | Assign cleanup prompts to apps and domains. Choose Automatic or a saved manual override. Add writing preferences scoped to an app, domain or language.                                                         |
| Voice commands        | Select text in another app, press the configured shortcut and speak an instruction. Review the result before replacing the selection. You can also capture a selection and type the instruction.               |
| Notes                 | Dictate into the editor, save drafts, restore revisions, import/export Markdown, apply saved prompts and ask questions about the note.                                                                         |
| Meetings              | Record microphone or system audio, or import a WAV. Transcribe locally, identify speakers, edit the timed transcript, then generate notes and review suggested action items.                                   |
| Search                | Search saved dictations, notes and connected documents. Ask a question to generate an answer with references to matching sources.                                                                              |
| Connections           | Import a local text file, a folder or an ICS calendar. Disconnect removes the cached documents.                                                                                                                |

Generation stays on your device. In **Post Processing**, choose Apple
Intelligence on a supported Mac, or **Custom** with a model server at
`localhost`, `127.0.0.1` or `::1`. Choose its model and enable post-processing in
**Advanced**. Commands, note transforms/chat, meeting summaries, search answers
and dictation cleanup all reject remote endpoints. Requests bypass environment
proxies and do not follow redirects. A local server remains responsible for its
own configuration. Plain snippets, vocabulary, notes and search need no model
server. Old remote connections are disabled; their cached documents remain
available until you disconnect them.

A **Dictionary** word reaches the model only when the model accepts a word
list, as Whisper does. Parakeet, Nemotron and other models without one get
close spellings corrected after transcription: “Omarchi” becomes “Omarchy”,
“Trimarchi” stays. Add such a spelling under **Heard as** and Handy replaces
it, as whole words, with any model. Words in **Advanced → Custom Words** are
used the same way.

Profiles are selected when dictation starts. Domain rules take precedence over
app rules, followed by the default profile. A manual override takes precedence
over automatic matching. Disabling or deleting its profile returns to automatic
matching. App identity is captured without reading field text; selected-text
commands capture a selection after you invoke them. Native adapters also read a
bounded plain-text field snapshot to detect intervening edits. Replacement
checks the original field, range and text. It never submits a message.

**Background spelling learning** and **Automatic commands** are separate
switches, both off initially. Learning observes only the field where Handy
successfully inserted its latest dictation, for at most 60 seconds. A stable
single-word spelling correction becomes local vocabulary, marked with its
origin and available for review or undo. Changes outside the inserted span,
rewrites, numbers, links and multiple changed words are ignored. It never
creates a replacement rule automatically. Secure or inaccessible fields are
excluded, as are fields larger than 64 KiB.

Automatic commands recognize conservative English instructions such as
“make this shorter” when text was selected at recording start. They open the
existing preview and require Apply before replacement. Ambiguous speech stays
dictation. The classifier itself uses no model or network request; producing
the edited text needs your local generation provider.

Notes retain their initial revision and the latest 49 revisions. Restoring one
creates a new revision. An unsaved draft is backed up locally. If a save fails
or another editor changes the note, the recovered draft pauses autosave so you
can keep a copy or resolve the conflict. Note chat retains the latest 40 messages.

Meeting recordings remain local. Interrupted capture or processing keeps saved
audio available for another transcription attempt. New microphone and system
tracks share a monotonic clock; silence preserves startup offsets and gaps.
Older recordings without clock metadata cannot be treated as synchronized.
Ordinary dictation and meeting capture cannot record at the same time.

Choose a local Sortformer GGUF in Meetings, or explicitly download the pinned
139 MB speaker model. Downloading weights sends no recording or transcript.
**Identify speakers** runs the model in a separate local worker. The bundled
choice supports up to four voices. Speaker IDs distinguish voices, not people's
names. Rename or merge speakers and correct individual transcript segments to
resolve mistakes. Overlapping speech retains all model speaker candidates and
is marked for review instead of attributing the words to one person. Transcript
edits mark generated notes stale; previously accepted action items survive.
Generated actions remain drafts until you confirm them.

Native capture uses ScreenCaptureKit/CoreAudio. System audio requires macOS 13
or later and recording permission. Selection commands use macOS Accessibility.
Support depends on the destination app exposing a writable text field.
Inaccessible fields and unavailable audio devices return an error. WAV import
does not require microphone or system-recording permission.

This code has not yet been exercised through the running workspace UI.
The local speaker worker has been tested on macOS with
two alternating synthetic voices; that does not establish accuracy on real
meetings. Workspace labels currently use English with the app's language
fallback. Cloud sync, accounts, OAuth connectors and rich HTML snippets are
outside this local implementation.
