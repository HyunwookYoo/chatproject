//! The five commands (spec 9.2). Each item takes one path: decode at the
//! boundary, then on the store worker limit, verify, store. The rate token
//! comes before the signature so a caller over its limit costs no verify.

use std::net::IpAddr;

use queue_wire::{
    AckItem, CommandName, CreateItem, DeleteItem, ItemResult, QueueId, RecvItem, SendItem, Signed,
    TS_SKEW_MS, decode_array, decode_ct, signing_bytes, verify,
};
use serde::de::DeserializeOwned;

use crate::limit::Limiter;
use crate::store::{Keys, RECV_BUDGET, Sent, Tx};

/// What every item of one request shares.
pub(crate) struct Ctx<'a> {
    pub(crate) now: u64,
    pub(crate) client: IpAddr,
    pub(crate) limiter: &'a Limiter,
    pub(crate) max_bytes: u64,
    /// JSON bytes a RECV answer still has room for.
    pub(crate) budget: usize,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(now: u64, client: IpAddr, limiter: &'a Limiter, max_bytes: u64) -> Self {
        Ctx {
            now,
            client,
            limiter,
            max_bytes,
            budget: RECV_BUDGET,
        }
    }
}

pub(crate) trait Command: Sized + std::marker::Send + 'static {
    const NAME: CommandName;
    type Item: DeserializeOwned;

    /// A per-request limit, checked before the body is read.
    fn admit(_limiter: &Limiter, _client: IpAddr, _now: u64) -> bool {
        true
    }

    /// `None`: the item is `bad` (malformed, or `ts` outside ±[`TS_SKEW_MS`]).
    fn decode(item: Self::Item, now: u64) -> Option<Self>;

    fn run(self, tx: &Tx, ctx: &mut Ctx) -> rusqlite::Result<ItemResult>;
}

/// The checks every signed item gets first.
struct Head {
    id: QueueId,
    ts: u64,
    sig: [u8; 64],
}

impl Head {
    fn decode(queue_id: &str, ts: u64, sig: &str, now: u64) -> Option<Head> {
        if ts.abs_diff(now) > TS_SKEW_MS {
            return None;
        }
        Some(Head {
            id: decode_array(queue_id).ok()?,
            ts,
            sig: decode_array(sig).ok()?,
        })
    }

    fn signed_by(&self, public: &[u8; 32], signed: &Signed) -> bool {
        verify(public, &signing_bytes(signed, &self.id, self.ts), &self.sig)
    }
}

pub(crate) struct Create {
    head: Head,
    keys: Keys,
}

impl Command for Create {
    const NAME: CommandName = CommandName::Create;
    type Item = CreateItem;

    fn decode(item: CreateItem, now: u64) -> Option<Self> {
        Some(Create {
            head: Head::decode(&item.queue_id, item.ts, &item.sig, now)?,
            keys: Keys {
                recipient_pub: decode_array(&item.recipient_pub).ok()?,
                sender_pub: decode_array(&item.sender_pub).ok()?,
            },
        })
    }

    fn run(self, tx: &Tx, ctx: &mut Ctx) -> rusqlite::Result<ItemResult> {
        if !ctx.limiter.allow_create(ctx.client, ctx.now) {
            return Ok(ItemResult::Limited);
        }
        let signed = Signed::Create {
            sender_pub: &self.keys.sender_pub,
        };
        if !self.head.signed_by(&self.keys.recipient_pub, &signed) {
            return Ok(ItemResult::Bad);
        }
        Ok(if tx.create(&self.head.id, &self.keys, ctx.now)? {
            ItemResult::Ok
        } else {
            ItemResult::Exists
        })
    }
}

pub(crate) struct Send {
    head: Head,
    ct: Vec<u8>,
}

impl Command for Send {
    const NAME: CommandName = CommandName::Send;
    type Item = SendItem;

    fn decode(item: SendItem, now: u64) -> Option<Self> {
        let head = Head::decode(&item.queue_id, item.ts, &item.sig, now)?;
        let ct = decode_ct(&item.ct).ok().filter(|ct| !ct.is_empty())?;
        Some(Send { head, ct })
    }

    fn run(self, tx: &Tx, ctx: &mut Ctx) -> rusqlite::Result<ItemResult> {
        let Some(keys) = tx.keys(&self.head.id)? else {
            return Ok(ItemResult::Gone);
        };
        // Before the verify: a holder of the queue id without the sender key
        // can then spend this queue's tokens, but never the worker's CPU.
        if !ctx.limiter.allow_send(&self.head.id, ctx.now) {
            return Ok(ItemResult::Limited);
        }
        if !self
            .head
            .signed_by(&keys.sender_pub, &Signed::Send { ct: &self.ct })
        {
            return Ok(ItemResult::Bad);
        }
        Ok(
            match tx.send(&self.head.id, &self.ct, ctx.now, ctx.max_bytes)? {
                Sent::Ok => ItemResult::Ok,
                Sent::Full => ItemResult::Full,
                Sent::OverCap => ItemResult::Limited,
            },
        )
    }
}

pub(crate) struct Recv {
    head: Head,
    since_seq: u64,
}

impl Command for Recv {
    const NAME: CommandName = CommandName::Recv;
    type Item = RecvItem;

    fn admit(limiter: &Limiter, client: IpAddr, now: u64) -> bool {
        limiter.allow_recv(client, now)
    }

    fn decode(item: RecvItem, now: u64) -> Option<Self> {
        Some(Recv {
            head: Head::decode(&item.queue_id, item.ts, &item.sig, now)?,
            since_seq: item.since_seq,
        })
    }

    fn run(self, tx: &Tx, ctx: &mut Ctx) -> rusqlite::Result<ItemResult> {
        let Some(keys) = tx.keys(&self.head.id)? else {
            return Ok(ItemResult::Gone);
        };
        let signed = Signed::Recv {
            since_seq: self.since_seq,
        };
        if !self.head.signed_by(&keys.recipient_pub, &signed) {
            return Ok(ItemResult::Bad);
        }
        Ok(
            match tx.recv(&self.head.id, self.since_seq, ctx.now, &mut ctx.budget)? {
                Some(entries) => ItemResult::Entries { entries },
                // This answer is full; the next RECV gets them.
                None => ItemResult::Limited,
            },
        )
    }
}

pub(crate) struct Ack {
    head: Head,
    upto_seq: u64,
}

impl Command for Ack {
    const NAME: CommandName = CommandName::Ack;
    type Item = AckItem;

    fn admit(limiter: &Limiter, client: IpAddr, now: u64) -> bool {
        limiter.allow_ack(client, now)
    }

    fn decode(item: AckItem, now: u64) -> Option<Self> {
        Some(Ack {
            head: Head::decode(&item.queue_id, item.ts, &item.sig, now)?,
            upto_seq: item.upto_seq,
        })
    }

    fn run(self, tx: &Tx, ctx: &mut Ctx) -> rusqlite::Result<ItemResult> {
        let Some(keys) = tx.keys(&self.head.id)? else {
            return Ok(ItemResult::Gone);
        };
        let signed = Signed::Ack {
            upto_seq: self.upto_seq,
        };
        if !self.head.signed_by(&keys.recipient_pub, &signed) {
            return Ok(ItemResult::Bad);
        }
        tx.ack(&self.head.id, self.upto_seq, ctx.now)?;
        Ok(ItemResult::Ok)
    }
}

pub(crate) struct Delete {
    head: Head,
}

impl Command for Delete {
    const NAME: CommandName = CommandName::Delete;
    type Item = DeleteItem;

    fn admit(limiter: &Limiter, client: IpAddr, now: u64) -> bool {
        limiter.allow_delete(client, now)
    }

    fn decode(item: DeleteItem, now: u64) -> Option<Self> {
        Some(Delete {
            head: Head::decode(&item.queue_id, item.ts, &item.sig, now)?,
        })
    }

    fn run(self, tx: &Tx, _ctx: &mut Ctx) -> rusqlite::Result<ItemResult> {
        // Already gone is what DELETE wants: `ok`.
        let Some(keys) = tx.keys(&self.head.id)? else {
            return Ok(ItemResult::Ok);
        };
        if !self.head.signed_by(&keys.recipient_pub, &Signed::Delete) {
            return Ok(ItemResult::Bad);
        }
        tx.delete(&self.head.id)?;
        Ok(ItemResult::Ok)
    }
}
