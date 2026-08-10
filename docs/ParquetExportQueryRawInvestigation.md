# Parquet Export `query_raw` Investigation (Resolved)

This document records the investigation into why the streaming
`client.query_raw()` path initially appeared unreliable inside
`export_parquet`, and the actual root cause discovered after bisecting.

## TL;DR (root cause found)

The "connection closed" errors during streaming export were **not** caused
by `query_raw` being unstable, async-fn state-machine layout, or any
tokio-opengauss bug. They were caused by a logic error in **our own drain
loop**:

> `tokio-opengauss::RowStream::poll_next` returns `Poll::Ready(None)` on
> `ReadyForQuery` (clean EOF), but subsequent calls return
> `Poll::Ready(Some(Err(Error::closed())))` rather than continuing to
> return `None`.

Our original code drained the first batch in one loop, then unconditionally
drained "the rest" in a second loop. When the result set was smaller than
`batch_size`, the first loop hit EOF, and the second loop's first
`stream.next().await` immediately returned `Err(closed)`.

**Fix:** track an `stream_exhausted` flag during the first drain; skip the
second drain loop when set. Result: streaming works 100% across all sizes,
memory drops to O(batch_size) (10M × 200B source peaks at 247 MB client RSS
vs 4.7 GB for the previous buffered fallback).

## How the root cause was found

### False starts (recorded so future investigators skip them)

Three hypotheses were tested and **eliminated** before the real cause was
identified:

1. **Async-fn boundary / Future size hypothesis** — disproved by
   `Box::pin(async { ... })` wrapping. The first time the wrap "fixed"
   the failure, it was actually because the wrap changed where the first
   drain hit EOF (different `batch_size` interaction).
2. **Multi-thread scheduler hypothesis** — disproved by switching main to
   `#[tokio::main(flavor = "current_thread")]`. Failures persisted.
3. **Buffered fallback (the prior solution)** — `client.query()` +
   `Vec<Row>` accumulation worked because it doesn't use `RowStream` at
   all, so the EOF-after-None quirk never surfaced.

### The bisection that pinned it

Two test cases with identical code, only SQL/result size differed:

| SQL rows | `batch_size` | First batch drains… | Second loop first poll | Result |
|---|---|---|---|---|
| 100 | 65536 (default) | 100 rows + EOF | `Err(closed)` | **fail** |
| 1000 | 100 | 100 rows, no EOF | `Some(Ok(row))` × 900, then EOF | **works** |

The discriminator was entirely whether the first drain reached EOF. That
pointed directly at the stream's post-EOF behaviour.

## The underlying protocol mechanic

For the curious — why does `RowStream` behave this way?

`RowStream::poll_next` delegates to `Responses::poll_next`
(`crates/tokio-opengauss/src/client.rs:47`):

```rust
impl Responses {
    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Result<Message, Error>> {
        loop {
            match self.cur.next().map_err(Error::parse)? {
                Some(Message::ErrorResponse(body)) => return Poll::Ready(Err(Error::db(body))),
                Some(message) => return Poll::Ready(Ok(message)),
                None => {}
            }
            match ready!(self.receiver.poll_next_unpin(cx)) {
                Some(messages) => self.cur = messages,
                None => return Poll::Ready(Err(Error::closed())),  // <-- line 57
            }
        }
    }
}
```

The `mpsc::Receiver<BackendMessages>` returns `None` when the matching
`Sender` (held inside the connection task) is dropped. The connection task
drops the sender once `request_complete = true` (i.e. after dispatching
the `ReadyForQuery` message that ends a query cycle):

```rust
// crates/tokio-opengauss/src/connection.rs, poll_read()
match response.sender.poll_ready(cx) {
    Poll::Ready(Ok(())) => {
        let _ = response.sender.start_send(messages);
        if !request_complete {
            self.responses.push_front(response);  // keep sender alive
        }
        // else: response (and its sender) drops at end of scope
    }
    // ...
}
```

So the sequence on a clean query end is:

1. Connection task receives `ReadyForQuery` from server, marks
   `request_complete = true`, sends the message batch through the channel,
   then drops the response (and sender).
2. Caller's `Responses::poll_next` consumes the channel message containing
   `ReadyForQuery`. `RowStream::poll_next` matches it and returns
   `Poll::Ready(None)` — clean EOF.
3. Caller polls again. `self.cur` is empty. `receiver.poll_next()` returns
   `None` (sender is gone). `Responses::poll_next` returns
   `Poll::Ready(Err(Error::closed()))`.

This is **by design** in tokio-opengauss — `Responses` is a single-shot
channel-per-request. The contract is "stop polling after the first `None`."
Our drain loop violated that contract.

## The fix

`tools/gaussdb-mcp/src/parquet_export.rs` `export_parquet`:

```rust
let mut first_batch: Vec<Row> = Vec::with_capacity(opts.batch_size);
let mut stream_exhausted = false;
while first_batch.len() < opts.batch_size {
    match stream.next().await {
        None => {
            stream_exhausted = true;
            break;
        }
        Some(Ok(r)) => first_batch.push(r),
        Some(Err(e)) => return Err(format!("query failed: {}", format_sql_error(&e))),
    }
}

// ... build schema + ArrowWriter, write first_batch ...

// Only continue draining if the first batch did NOT hit EOF.
if !stream_exhausted {
    let mut buf: Vec<Row> = Vec::with_capacity(opts.batch_size);
    while let Some(next) = stream.next().await {
        // ...
    }
}
```

The `stream_exhausted` flag is the entire fix. The comment in the source
explains why it's necessary so a future maintainer doesn't "simplify" it
away.

## Validation

- 20/20 consecutive runs at 1k rows (previously 100% fail)
- 1k / 10k / 100k / 1M / 10M rows all succeed
- Multi-batch (1000 rows, `batch_size=100`) works
- 10M rows × ~200 B source: **247 MB peak RSS** (was 4710 MB with buffered
  fallback, was failing outright with naive streaming)
- Wall clock parity with CSV at all sizes

## Lessons

1. **Read the underlying stream's EOF contract carefully.** `Stream::next`
   returning `None` does not imply subsequent calls also return `None` —
   some stream implementations are single-shot.
2. **Bisect by input shape, not by code shape.** The bisection that found
   the root cause varied the SQL/result-size while keeping the code fixed.
   Earlier bisections varied the code (Box::pin, runtime flavor) while
   keeping the input fixed, which led down wrong paths because the
   "fail/success" boundary was the EOF condition, not the code change.
3. **Confirmation bias is expensive.** The first time Box::pin "fixed"
   the issue, it was actually a different `batch_size` interaction. The
   premature conclusion sent the investigation down a 2-day wrong path.
   Should have re-run the original failing case after every "fix" to
   verify it was the code change, not a coincidental state change.
