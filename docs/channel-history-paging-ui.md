Group history requests one recent remote page when opened. Older pages require an explicit action using the opaque returned cursor. Preparing, blocked, offline and reset states permit manual retry; unsupported peers are not retried automatically. Closing or changing scope discards pending UI completions.

The remote paging API is separate from the existing local history display window: the dialog still projects at most 500 local messages. Paging does not imply that the UI displays the entire remote backlog. Account history remains local only.
