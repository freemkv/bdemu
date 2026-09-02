# `short_response_zeroes_the_untouched_tail`

A response shorter than the host's transfer length must leave the rest of
the host buffer ZEROED, not holding whatever the host had there before.
Catches the mutation that drops the tail `write_bytes` while keeping the
copy — stale host memory past a short INQUIRY/READ_TOC response is
indistinguishable, to the host, from bytes the drive returned. Also pins
the residual the libfreemkv client uses to compute how much was
transferred.
