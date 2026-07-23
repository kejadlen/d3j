# json-sequence

The paper's worked example from Figures 2 and 3. Base is `[1, 2, 3]`;
left inserts `4, 5` after `2`; right replaces `2` with `6`.

The edits are independent — left touches the slot after `2`, right
relabels `2` — so the paper argues the correct structural merge is
`[1, 6, 4, 5, 3]`. Line-oriented tools conflict because the changed
lines overlap, and mergiraf falls back to a line conflict here. This is
the flagship case d3j aims to merge cleanly.
