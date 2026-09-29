# Answering questions about a YouTube video

1. **Get the transcript** with `youtube_transcript` (`video` = the URL or id). It is saved
   as a case file; read it with `read_file`. Read a long transcript in parts, to the end,
   rather than answering from the opening only.
2. **No English track?** Call `youtube_caption_languages`, then `youtube_transcript` with
   `lang` set to an available track, and `translate = "en"` if the owner needs English.
3. **Answer from the transcript.** Cite a `[MM:SS]` timestamp for every claim. If the
   captions garble a word or a number, say so; do not guess what was "probably" said.
   Auto-generated captions often get numbers, names and tickers wrong: flag any figure
   the answer relies on.
4. **If the download fails**, report the error as given and stop. Never fill in from
   memory what a video "probably" says.
   - `TranscriptsDisabled` / `NoTranscriptFound`: the video has no captions.
   - `RequestBlocked` / `IpBlocked`: YouTube blocked the server's address; the owner can
     set a proxy (`YT_PROXY_URL` in the plugin's `config.toml`).
   - `AgeRestricted`: the video needs a signed-in session and cannot be read.
5. **Quote sparingly.** Short excerpts that support a point, never the whole transcript.
6. When the owner asked for a summary or an answer and nothing else is pending, finish
   with `complete`, putting the answer in `summary`.
