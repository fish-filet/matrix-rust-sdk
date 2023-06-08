use std::sync::Arc;

use anyhow::Context;
use matrix_sdk::{
    notification_settings::RoomNotificationMode as SdkRoomNotificationMode,
    ruma::events::push_rules::PushRulesEvent, Client as MatrixClient,
};
use ruma::{
    push::{PredefinedOverrideRuleId, PredefinedUnderrideRuleId, Ruleset},
    RoomId,
};
use tokio::sync::RwLock;

use crate::error::NotificationSettingsError;

#[derive(Clone, uniffi::Enum)]
pub enum RoomNotificationMode {
    AllMessages,
    MentionsAndKeywordsOnly,
    Mute,
}

impl From<SdkRoomNotificationMode> for RoomNotificationMode {
    fn from(value: SdkRoomNotificationMode) -> Self {
        match value {
            SdkRoomNotificationMode::AllMessages => Self::AllMessages,
            SdkRoomNotificationMode::MentionsAndKeywordsOnly => Self::MentionsAndKeywordsOnly,
            SdkRoomNotificationMode::Mute => Self::Mute,
        }
    }
}

pub trait NotificationSettingsDelegate: Sync + Send {
    fn notification_settings_did_change(&self);
}

#[derive(Clone, uniffi::Record)]
pub struct RoomNotificationSettings {
    mode: RoomNotificationMode,
    is_default: bool,
}

impl RoomNotificationSettings {
    fn new(mode: RoomNotificationMode, is_default: bool) -> Self {
        RoomNotificationSettings { mode, is_default }
    }
}

#[derive(Clone, uniffi::Enum)]
pub enum PredefinedRuleId {
    /// `.m.rule.call`
    Call,

    /// `.m.rule.encrypted_room_one_to_one`
    EncryptedRoomOneToOne,

    IsRoomMention,

    IsUserMention,

    /// `.m.rule.room_one_to_one`
    RoomOneToOne,

    /// `.m.rule.message`
    Message,

    /// `.m.rule.encrypted`
    Encrypted,
}

#[derive(Clone, uniffi::Object)]
pub struct NotificationSettings {
    pub(crate) sdk_client: MatrixClient,
    push_rules: Arc<RwLock<Ruleset>>,
    delegate: Arc<RwLock<Option<Box<dyn NotificationSettingsDelegate>>>>,
}

impl NotificationSettings {
    pub(crate) fn new(sdk_client: MatrixClient) -> Self {
        let push_rules = Arc::new(tokio::sync::RwLock::new(Ruleset::new()));
        let delegate: Arc<RwLock<Option<Box<dyn NotificationSettingsDelegate>>>> =
            Arc::new(RwLock::new(None));

        // Listen for PushRulesEvent
        let push_rules_clone = push_rules.to_owned();
        let delegate_clone = delegate.to_owned();
        sdk_client.add_event_handler(move |ev: PushRulesEvent| {
            let push_rules = push_rules_clone.clone();
            let delegate = delegate_clone.clone();
            async move {
                *push_rules.write().await = ev.content.global;
                if let Some(delegate) = delegate.read().await.as_ref() {
                    delegate.notification_settings_did_change();
                }
            }
        });

        Self { sdk_client, push_rules, delegate }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl NotificationSettings {
    /// Sets a delegate.
    pub async fn set_delegate(&self, delegate: Option<Box<dyn NotificationSettingsDelegate>>) {
        *self.delegate.write().await = delegate;
    }

    /// Gets the notification mode for a room.
    ///
    /// # Arguments
    ///
    /// * `room_id` - A room ID
    pub async fn get_room_notification_mode(
        &self,
        room_id: String,
    ) -> Result<RoomNotificationSettings, NotificationSettingsError> {
        let ruleset = &*self.push_rules.read().await;
        let notification_settings = self.sdk_client.notification_settings();
        let parsed_room_id = RoomId::parse(&room_id)
            .map_err(|_e| NotificationSettingsError::InvalidRoomId(room_id))?;
        // Get the current user defined mode for this room
        if let Some(mode) =
            notification_settings.get_user_defined_room_notification_mode(&parsed_room_id, ruleset)
        {
            return Ok(RoomNotificationSettings::new(mode.into(), false));
        }

        // If the user didn't defined a notification mode, return the default one for
        // this room
        let room = self
            .sdk_client
            .get_room(&parsed_room_id)
            .context("Room not found")
            .map_err(|_| NotificationSettingsError::RoomNotFound)?;

        let is_encrypted = room.is_encrypted().await.unwrap_or(false);
        let members_count = room.joined_members_count();

        let mode = notification_settings.get_default_room_notification_mode(
            is_encrypted,
            members_count,
            ruleset,
        );
        Ok(RoomNotificationSettings::new(mode.into(), true))
    }

    /// Gets the default notification mode for a room.
    ///
    /// # Arguments
    ///
    /// * `is_encrypted` - A `bool` indicating whether the room is encrypted
    /// * `members_count` - The members count
    pub async fn get_default_room_notification_mode(
        &self,
        is_encrypted: bool,
        members_count: u64,
    ) -> RoomNotificationMode {
        let ruleset = &*self.push_rules.read().await;
        let notification_settings = self.sdk_client.notification_settings();
        let mode = notification_settings.get_default_room_notification_mode(
            is_encrypted,
            members_count,
            ruleset,
        );
        mode.into()
    }

    /// Sets the notification mode for a room.
    ///
    /// # Arguments
    ///
    /// * `room_id` - A room ID
    /// * `mode` - A `RoomNotificationMode`
    pub async fn set_room_notification_mode(
        &self,
        room_id: String,
        mode: RoomNotificationMode,
    ) -> Result<(), NotificationSettingsError> {
        let mut ruleset = self.push_rules.read().await.clone();
        let notification_settings = self.sdk_client.notification_settings();
        let mode = match mode {
            RoomNotificationMode::AllMessages => SdkRoomNotificationMode::AllMessages,
            RoomNotificationMode::MentionsAndKeywordsOnly => {
                SdkRoomNotificationMode::MentionsAndKeywordsOnly
            }
            RoomNotificationMode::Mute => SdkRoomNotificationMode::Mute,
        };
        let parsed_room_idom_id = RoomId::parse(&room_id)
            .map_err(|_e| NotificationSettingsError::InvalidRoomId(room_id))?;
        notification_settings
            .set_room_notification_mode(&parsed_room_idom_id, mode, &mut ruleset)
            .await?;
        *self.push_rules.write().await = ruleset;
        Ok(())
    }

    /// Restores the default notification mode for a room
    ///
    /// # Arguments
    ///
    /// * `room_id` - A room ID
    pub async fn restore_default_room_notification_mode(
        &self,
        room_id: String,
    ) -> Result<(), NotificationSettingsError> {
        let mut ruleset = self.push_rules.read().await.clone();
        let notification_settings = self.sdk_client.notification_settings();
        let parsed_room_idom_id = RoomId::parse(&room_id)
            .map_err(|_e| NotificationSettingsError::InvalidRoomId(room_id))?;
        notification_settings
            .delete_user_defined_room_rules(&parsed_room_idom_id, &mut ruleset)
            .await?;
        *self.push_rules.write().await = ruleset;
        Ok(())
    }

    /// Get whether some enabled keyword rules exist.
    pub async fn contains_keywords_rules(&self) -> bool {
        let ruleset = &*self.push_rules.read().await;
        self.sdk_client.notification_settings().contains_keyword_rules(ruleset)
    }

    /// Unmute a room.
    ///     
    /// # Arguments
    ///
    /// * `room_id` - A room ID
    pub async fn unmute_room(&self, room_id: String) -> Result<(), NotificationSettingsError> {
        let mut ruleset = self.push_rules.read().await.clone();
        let notification_settings = self.sdk_client.notification_settings();
        let parsed_room_idom_id = RoomId::parse(&room_id)
            .map_err(|_e| NotificationSettingsError::InvalidRoomId(room_id))?;
        notification_settings.unmute_room(&parsed_room_idom_id, &mut ruleset).await?;
        *self.push_rules.write().await = ruleset;
        Ok(())
    }

    /// Get whether a predefined rule is enabled.
    ///
    /// # Arguments
    ///
    /// * `rule_id` - A `PredefinedRuleId`
    pub async fn is_predefined_rule_enabled(
        &self,
        rule_id: PredefinedRuleId,
    ) -> Result<bool, NotificationSettingsError> {
        let ruleset = self.push_rules.read().await.clone();
        let notification_settings = self.sdk_client.notification_settings();

        let enabled = match rule_id {
            PredefinedRuleId::Call => notification_settings
                .is_predefined_underride_rule_enabled(PredefinedUnderrideRuleId::Call, &ruleset),
            PredefinedRuleId::EncryptedRoomOneToOne => notification_settings
                .is_predefined_underride_rule_enabled(
                    PredefinedUnderrideRuleId::EncryptedRoomOneToOne,
                    &ruleset,
                ),
            PredefinedRuleId::IsRoomMention => notification_settings
                .is_predefined_override_rule_enabled(
                    ruma::push::PredefinedOverrideRuleId::IsRoomMention,
                    &ruleset,
                ),
            PredefinedRuleId::IsUserMention => notification_settings
                .is_predefined_override_rule_enabled(
                    ruma::push::PredefinedOverrideRuleId::IsUserMention,
                    &ruleset,
                ),
            PredefinedRuleId::RoomOneToOne => notification_settings
                .is_predefined_underride_rule_enabled(
                    PredefinedUnderrideRuleId::RoomOneToOne,
                    &ruleset,
                ),
            PredefinedRuleId::Message => notification_settings
                .is_predefined_underride_rule_enabled(PredefinedUnderrideRuleId::Message, &ruleset),
            PredefinedRuleId::Encrypted => notification_settings
                .is_predefined_underride_rule_enabled(
                    PredefinedUnderrideRuleId::Encrypted,
                    &ruleset,
                ),
        };
        Ok(enabled?)
    }

    /// Set whether a predefined rule is enabled.
    ///
    /// # Arguments
    ///
    /// * `rule_id` - A `PredefinedRuleId`
    /// * `enabled` - A `bool` indicating whether the rule should be activated
    pub async fn set_predefined_rule_enabled(
        &self,
        rule_id: PredefinedRuleId,
        enabled: bool,
    ) -> Result<(), NotificationSettingsError> {
        let mut ruleset = self.push_rules.read().await.clone();
        let notification_settings = self.sdk_client.notification_settings();

        match rule_id {
            PredefinedRuleId::Call => {
                notification_settings
                    .set_predefined_underride_rule_enabled(
                        PredefinedUnderrideRuleId::Call,
                        enabled,
                        &mut ruleset,
                    )
                    .await?
            }
            PredefinedRuleId::Encrypted => {
                notification_settings
                    .set_predefined_underride_rule_enabled(
                        PredefinedUnderrideRuleId::Encrypted,
                        enabled,
                        &mut ruleset,
                    )
                    .await?
            }
            PredefinedRuleId::EncryptedRoomOneToOne => {
                notification_settings
                    .set_predefined_underride_rule_enabled(
                        PredefinedUnderrideRuleId::EncryptedRoomOneToOne,
                        enabled,
                        &mut ruleset,
                    )
                    .await?
            }
            PredefinedRuleId::IsRoomMention => {
                notification_settings
                    .set_predefined_override_rule_enabled(
                        PredefinedOverrideRuleId::IsRoomMention,
                        enabled,
                        &mut ruleset,
                    )
                    .await?
            }
            PredefinedRuleId::IsUserMention => {
                notification_settings
                    .set_predefined_override_rule_enabled(
                        PredefinedOverrideRuleId::IsUserMention,
                        enabled,
                        &mut ruleset,
                    )
                    .await?
            }
            PredefinedRuleId::Message => {
                notification_settings
                    .set_predefined_underride_rule_enabled(
                        PredefinedUnderrideRuleId::Message,
                        enabled,
                        &mut ruleset,
                    )
                    .await?
            }
            PredefinedRuleId::RoomOneToOne => {
                notification_settings
                    .set_predefined_underride_rule_enabled(
                        PredefinedUnderrideRuleId::RoomOneToOne,
                        enabled,
                        &mut ruleset,
                    )
                    .await?
            }
        };
        Ok(())
    }
}
