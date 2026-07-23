# rust-independent-methods

Each branch appends a different method to the same `impl` block: left
adds `goodbye`, right adds `ping`. The additions are independent, so a
clean merge keeping all three methods is expected. A textual tool may
conflict because both edits touch the closing lines of the block; a
structural tool should not.
