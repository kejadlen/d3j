# json-independent-keys

Each branch adds a different key to an object: left adds `"b"`, right
adds `"c"`. The insertions land in the same unordered collection but do
not collide, so both tools should merge cleanly to an object with all
three keys. A baseline case where structural merge and mergiraf are
expected to agree.
