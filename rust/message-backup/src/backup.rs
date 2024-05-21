//
// Copyright (C) 2023 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use std::collections::{hash_map, HashMap};
use std::fmt::Debug;

use crate::backup::account_data::{AccountData, AccountDataError};
use crate::backup::call::{Call, CallError, MaybeWithCall};
use crate::backup::chat::{ChatData, ChatError, ChatItemError};
use crate::backup::frame::{CallId, ChatId, RecipientId};
use crate::backup::method::{Contains, KeyExists, Map as _, Method, Store, ValidateOnly};
use crate::backup::recipient::{RecipientData, RecipientError};
use crate::backup::sticker::{PackId as StickerPackId, StickerId, StickerPack, StickerPackError};
use crate::backup::time::Timestamp;
use crate::proto::backup as proto;
use crate::proto::backup::frame::Item as FrameItem;

mod account_data;
mod call;
mod chat;
mod file;
mod frame;
pub(crate) mod method;
mod recipient;
mod sticker;
mod time;

pub struct PartialBackup<M: Method> {
    meta: BackupMeta,
    account_data: Option<M::Value<AccountData<M>>>,
    recipients: M::Map<RecipientId, RecipientData<M>>,
    chats: HashMap<ChatId, ChatData<M>>,
    calls: M::Map<CallId, Call>,
    sticker_packs: HashMap<StickerPackId, StickerPack>,
}

#[derive(Debug)]
pub struct Backup {
    pub meta: BackupMeta,
    pub account_data: Option<AccountData<Store>>,
    pub recipients: HashMap<RecipientId, RecipientData>,
    pub chats: HashMap<ChatId, ChatData>,
    pub calls: HashMap<CallId, Call>,
    pub sticker_packs: HashMap<StickerPackId, StickerPack>,
}

#[derive(Debug)]
pub struct BackupMeta {
    /// The version of the backup format being parsed.
    pub version: u64,
    /// When the backup process started.
    pub backup_time: Timestamp,
    /// What purpose the backup was intended for.
    pub purpose: Purpose,
}

#[repr(u8)]
#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    num_enum::TryFromPrimitive,
    strum::EnumString,
    strum::Display,
    strum::IntoStaticStr,
)]
pub enum Purpose {
    /// Intended for immediate transfer from one device to another.
    #[strum(
        serialize = "device_transfer",
        serialize = "device-transfer",
        serialize = "transfer"
    )]
    DeviceTransfer = 0,
    /// For remote storage and restoration at a later time.
    #[strum(
        serialize = "remote_backup",
        serialize = "remote-backup",
        serialize = "backup"
    )]
    RemoteBackup = 1,
}

impl From<PartialBackup<Store>> for Backup {
    fn from(value: PartialBackup<Store>) -> Self {
        let PartialBackup {
            meta,
            account_data,
            recipients,
            chats,
            calls,
            sticker_packs,
        } = value;

        Self {
            meta,
            account_data,
            recipients,
            chats,
            calls,
            sticker_packs,
        }
    }
}

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum ValidationError {
    /// Frame.item is a oneof but has no value
    EmptyFrame,
    /// multiple AccountData frames found
    MultipleAccountData,
    /// AccountData error: {0}
    AccountData(#[from] AccountDataError),
    /// {0}
    RecipientError(#[from] RecipientFrameError),
    /// {0}
    ChatError(#[from] ChatFrameError),
    /// {0}
    CallError(#[from] CallFrameError),
    /// {0}
    StickerError(#[from] StickerError),
}

#[derive(Debug, displaydoc::Display, thiserror::Error)]
/// chat frame {0:?} error: {1}
pub struct ChatFrameError(ChatId, ChatError);

/// call data {0:?} error: {1}
#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub struct CallFrameError(CallId, CallError);

/// Like [`TryFrom`] but with an extra context argument.
///
/// Implements fallible conversions from `T` into `Self` with an additional
/// "context" argument.
trait TryFromWith<T, C>: Sized {
    type Error;

    /// Uses additional context to convert `item` into an instance of `Self`.
    ///
    /// If the lookup fails, an instance of `Self::Error` is returned.
    fn try_from_with(item: T, context: &C) -> Result<Self, Self::Error>;
}

/// Like [`TryInto`] but with an extra context argument.
///
/// This trait is blanket-implemented for types that implement [`TryFromWith`].
/// Its only purpose is to offer the more convenient `x.try_into_with(c)` as
/// opposed to `Y::try_from_with(x, c)`.
trait TryIntoWith<T, C>: Sized {
    type Error;

    /// Uses additional context to convert `self` into an instance of `T`.
    ///
    /// If the lookup fails, an instance of `Self::Error` is returned.
    fn try_into_with(self, context: &C) -> Result<T, Self::Error>;
}

impl<A, B: TryFromWith<A, C>, C> TryIntoWith<B, C> for A {
    type Error = B::Error;
    fn try_into_with(self, context: &C) -> Result<B, Self::Error> {
        B::try_from_with(self, context)
    }
}

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum StickerError {
    /// pack ID is invalid
    InvalidId,
    /// multiple sticker packs for ID {0:?}
    DuplicateId(StickerPackId),
    /// for pack {0:?}: {1}
    PackError(StickerPackId, StickerPackError),
}

trait WithId {
    type Id;
    fn id(&self) -> Self::Id;
}

/// recipient {0:?} error: {1}
#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub struct RecipientFrameError(RecipientId, RecipientError);

impl PartialBackup<ValidateOnly> {
    pub fn new_validator(value: proto::BackupInfo, purpose: Purpose) -> Self {
        Self::new(value, purpose)
    }
}

impl PartialBackup<Store> {
    pub fn new_store(value: proto::BackupInfo, purpose: Purpose) -> Self {
        Self::new(value, purpose)
    }
}

impl<M: Method> PartialBackup<M> {
    pub fn new(value: proto::BackupInfo, purpose: Purpose) -> Self {
        let proto::BackupInfo {
            version,
            backupTimeMs,
            special_fields: _,
        } = value;

        let meta = BackupMeta {
            version,
            backup_time: Timestamp::from_millis(backupTimeMs, "BackupInfo.backupTimeMs"),
            purpose,
        };

        Self {
            meta,
            account_data: None,
            recipients: Default::default(),
            chats: Default::default(),
            calls: Default::default(),
            sticker_packs: HashMap::new(),
        }
    }

    pub fn add_frame(&mut self, frame: proto::Frame) -> Result<(), ValidationError> {
        self.add_frame_item(frame.item.ok_or(ValidationError::EmptyFrame)?)
    }

    fn add_frame_item(&mut self, item: FrameItem) -> Result<(), ValidationError> {
        match item {
            FrameItem::Account(account_data) => self.add_account_data(account_data),
            FrameItem::Recipient(recipient) => self.add_recipient(recipient).map_err(Into::into),
            FrameItem::Chat(chat) => self.add_chat(chat).map_err(Into::into),
            FrameItem::ChatItem(chat_item) => self.add_chat_item(chat_item).map_err(Into::into),
            FrameItem::StickerPack(sticker_pack) => {
                self.add_sticker_pack(sticker_pack).map_err(Into::into)
            }
        }
    }

    fn add_account_data(
        &mut self,
        account_data: proto::AccountData,
    ) -> Result<(), ValidationError> {
        if self.account_data.is_some() {
            return Err(ValidationError::MultipleAccountData);
        }
        let account_data = account_data.try_into()?;
        self.account_data = Some(M::value(account_data));
        Ok(())
    }

    fn add_recipient(&mut self, recipient: proto::Recipient) -> Result<(), RecipientFrameError> {
        let id = recipient.id();
        let err_with_id = |e| RecipientFrameError(id, e);
        let recipient = recipient.try_into().map_err(err_with_id)?;
        self.recipients
            .insert(id, recipient)
            .map_err(|KeyExists| err_with_id(RecipientError::DuplicateRecipient))
    }

    fn add_chat(&mut self, chat: proto::Chat) -> Result<(), ChatFrameError> {
        let id = chat.id();

        let chat = chat
            .try_into_with(&self.recipients)
            .map_err(|e| ChatFrameError(id, e))?;
        match self.chats.entry(id) {
            hash_map::Entry::Occupied(_) => Err(ChatFrameError(id, ChatError::DuplicateId)),
            hash_map::Entry::Vacant(v) => {
                let _ = v.insert(chat);
                Ok(())
            }
        }
    }

    fn add_chat_item(&mut self, chat_item: proto::ChatItem) -> Result<(), ValidationError> {
        let chat_id = ChatId(chat_item.chatId);

        let chat_data = match self.chats.entry(chat_id) {
            hash_map::Entry::Occupied(o) => o.into_mut(),
            hash_map::Entry::Vacant(_) => {
                return Err(ChatFrameError(chat_id, ChatItemError::NoChatForItem.into()).into())
            }
        };

        let MaybeWithCall {
            item: chat_item_data,
            call,
        } = chat_item
            .try_into_with(&ConvertContext {
                recipients: &self.recipients,
                calls: &self.calls,
                chats: &(),
                meta: &self.meta,
            })
            .map_err(|e: ChatItemError| ChatFrameError(chat_id, e.into()))?;

        // Delay updates to state until everything has been fallibly converted.
        if let Some(call) = call {
            let call_id = call.id;
            self.calls
                .insert(call_id, call)
                .map_err(|KeyExists| CallFrameError(call_id, CallError::DuplicateId))?
        }

        chat_data.items.extend([chat_item_data]);

        Ok(())
    }

    fn add_sticker_pack(&mut self, sticker_pack: proto::StickerPack) -> Result<(), StickerError> {
        let id = sticker_pack
            .packId
            .as_slice()
            .try_into()
            .map_err(|_| StickerError::InvalidId)?;
        let pack =
            StickerPack::try_from(sticker_pack).map_err(|e| StickerError::PackError(id, e))?;

        match self.sticker_packs.entry(id) {
            hash_map::Entry::Occupied(_) => Err(StickerError::DuplicateId(id)),
            hash_map::Entry::Vacant(v) => {
                v.insert(pack);
                Ok(())
            }
        }
    }
}

/// Context for converting proto types via [`TryFromWith`].
///
/// This is used as the concrete "context" type for the [`TryFromWith`]
/// implementations below.
pub(super) struct ConvertContext<'a, Recipients, Calls, Chats> {
    recipients: &'a Recipients,
    calls: &'a Calls,
    chats: &'a Chats,
    meta: &'a BackupMeta,
}

impl<R: Contains<RecipientId>, C, Ch> Contains<RecipientId> for ConvertContext<'_, R, C, Ch> {
    fn contains(&self, key: &RecipientId) -> bool {
        self.recipients.contains(key)
    }
}

impl<R, C: Contains<CallId>, Ch> Contains<CallId> for ConvertContext<'_, R, C, Ch> {
    fn contains(&self, key: &CallId) -> bool {
        self.calls.contains(key)
    }
}

impl<R, C, Ch: Contains<ChatId>> Contains<ChatId> for ConvertContext<'_, R, C, Ch> {
    fn contains(&self, key: &ChatId) -> bool {
        self.chats.contains(key)
    }
}

impl<M: Method> Contains<(StickerPackId, StickerId)> for HashMap<StickerPackId, StickerPack<M>> {
    fn contains(&self, (pack_id, sticker_id): &(StickerPackId, StickerId)) -> bool {
        self.get(pack_id)
            .is_some_and(|pack| pack.stickers.contains(sticker_id))
    }
}

impl<'a, R, C, Ch> AsRef<BackupMeta> for ConvertContext<'a, R, C, Ch> {
    fn as_ref(&self) -> &BackupMeta {
        self.meta
    }
}

#[cfg(test)]
mod test {
    use assert_matches::assert_matches;
    use test_case::{test_case, test_matrix};

    use crate::backup::chat::test::ChatItemTestData;

    use super::*;

    pub(crate) trait ProtoTestData {
        fn test_data() -> Self;
    }

    pub(crate) trait ProtoTestDataId<T = u64> {
        const TEST_ID: T;
    }

    impl ProtoTestDataId for proto::Chat {
        const TEST_ID: u64 = 22222;
    }

    impl ProtoTestData for proto::Chat {
        fn test_data() -> Self {
            Self {
                id: Self::TEST_ID,
                recipientId: proto::Recipient::TEST_ID,
                ..Default::default()
            }
        }
    }
    trait TestDataWrongAuthor {
        fn test_data_wrong_author() -> Self;
    }

    impl TestDataWrongAuthor for proto::ChatItem {
        fn test_data_wrong_author() -> Self {
            Self {
                authorId: proto::Recipient::TEST_ID + 1,
                ..Self::test_data()
            }
        }
    }

    trait TestPartialBackupMethod: Method + Sized {
        fn empty() -> PartialBackup<Self> {
            PartialBackup::new(proto::BackupInfo::new(), Purpose::RemoteBackup)
        }

        fn fake() -> PartialBackup<Self> {
            Self::fake_with([
                proto::Recipient::test_data().into(),
                proto::Chat::test_data().into(),
                proto::ChatItem::test_data().into(),
                proto::ChatItem::test_data_with_call().into(),
            ])
        }

        fn fake_with(frames: impl IntoIterator<Item = proto::frame::Item>) -> PartialBackup<Self> {
            let mut backup = Self::empty();

            for frame in frames {
                backup.add_frame_item(frame).expect("can add one");
            }
            backup
        }
    }

    impl<M: Method + Sized> TestPartialBackupMethod for M {}

    #[test_matrix(
        (ValidateOnly::fake(), Store::fake()),
        (proto::Recipient::test_data(), proto::Chat::test_data(), proto::ChatItem::test_data_with_call())
    )]
    fn rejects_duplicate_id<M: Method>(mut partial: PartialBackup<M>, item: impl Into<FrameItem>) {
        let err = partial.add_frame_item(item.into()).unwrap_err().to_string();
        assert!(err.contains("multiple"), "error was {err}");
    }

    #[test_matrix(
        (ValidateOnly::empty(), Store::empty()),
        (proto::Chat::test_data(), proto::ChatItem::test_data_with_call())
    )]
    #[test_case(
        ValidateOnly::fake_with([proto::Recipient::test_data().into()]),
        proto::ChatItem::test_data(); "missing chat item conversation"
    )]
    #[test_matrix(
        (ValidateOnly::fake(), Store::fake()),
        proto::ChatItem::test_data_wrong_author()
    )]
    fn rejects_missing_foreign_key<M: Method>(
        mut partial: PartialBackup<M>,
        item: impl Into<FrameItem>,
    ) {
        let frame = proto::Frame {
            item: Some(item.into()),
            ..Default::default()
        };

        let err = partial.add_frame(frame).unwrap_err().to_string();
        assert!(err.contains("no record"), "error was {err}");
    }

    #[test_case(ValidateOnly::empty())]
    #[test_case(Store::empty())]
    fn rejects_multiple_account_data(mut partial: PartialBackup<impl Method>) {
        partial
            .add_frame_item(proto::AccountData::test_data().into())
            .expect("accepts first");

        assert_matches!(
            partial.add_frame_item(proto::AccountData::test_data().into()),
            Err(ValidationError::MultipleAccountData)
        );
    }
}
