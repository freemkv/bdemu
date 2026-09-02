# `zero_fill_tail` — why the reused read buffer needs re-zeroing

`buf` in the capture read loop is reused across chunks for performance,
rather than allocating a fresh `vec![0u8; ..]` each iteration. That reuse
means bytes a short read did not fill still hold the *previous* chunk's
data, not zeros.

After an `Ok(transferred)` read, `zero_fill_tail` zeroes `buf[transferred..]`
so the fixture records zeros for the untransferred tail — matching the
semantics the old per-iteration `vec![0u8; ..]` allocation gave for free,
without paying its allocation cost on every chunk.

`transferred` is clamped to `buf.len()` before slicing so a transport that
over-reports the byte count cannot panic on an out-of-bounds slice.
