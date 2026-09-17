use actix_web::{HttpResponse, ResponseError};
use deadpool_postgres::PoolError;
use derive_more::{Display, From};

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Display, From)]
pub enum ApiError {
    PoolError(PoolError),
    PGError(tokio_postgres::error::Error),
    SerdeJsonError(serde_json::Error),
    #[from(ignore)]
    GroupCreationError(tokio_postgres::error::Error),
    #[from(ignore)]
    UpdateGroupMemberError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetGroupError(tokio_postgres::error::Error),
    #[from(ignore)]
    AddMemberError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetGroupDataError(tokio_postgres::error::Error),
    #[from(ignore)]
    DeleteGroupMemberError(tokio_postgres::error::Error),
    #[from(ignore)]
    RenameGroupError(tokio_postgres::error::Error),
    #[from(ignore)]
    IsMemberBlockedError(tokio_postgres::error::Error),
    #[from(ignore)]
    BlockGroupMemberError(tokio_postgres::error::Error),
    #[from(ignore)]
    UnblockGroupMemberError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetBlockedMembersError(tokio_postgres::error::Error),
    MemberBlockedError,
    DemoGroupReadOnlyError,
    #[from(ignore)]
    SeedError(String),
    #[from(ignore)]
    RerollGroupTokenError(tokio_postgres::error::Error),
    #[from(ignore)]
    DeleteGroupError(tokio_postgres::error::Error),
    #[from(ignore)]
    IsMemberInGroupError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetSkillsDataError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetMemberColorsError(tokio_postgres::error::Error),
    #[from(ignore)]
    UpsertMemberMeshError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetMemberMeshError(tokio_postgres::error::Error),
    GroupFullError,
    UreqError(ureq::Error),
    GroupMemberValidationError(String),
    #[from(ignore)]
    CreateAccountError(tokio_postgres::error::Error),
    UsernameAlreadyRegisteredError,
    #[from(ignore)]
    GetAccountError(tokio_postgres::error::Error),
    #[from(ignore)]
    CreateAccountSessionError(tokio_postgres::error::Error),
    InvalidCredentialsError,
    AccountDisabledError,
    #[from(ignore)]
    #[display("{_0}: {_1}")]
    AdminDbError(String, tokio_postgres::error::Error),
    AdminNotFoundError,
    AdminRateLimitedError,
    #[from(ignore)]
    #[display("{_0}")]
    AdminInvalidRequestError(String),
    #[from(ignore)]
    DiscordOAuthError(String),
    DiscordIdAlreadyLinkedError,
    #[from(ignore)]
    CreateCharacterError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetCharacterError(tokio_postgres::error::Error),
    #[from(ignore)]
    DeleteCharacterError(tokio_postgres::error::Error),
    CharacterDenylistedError,
    CharacterCapReachedError,
    CharacterNotFoundError,
    CharacterAlreadyInGroupError,
    GroupNotFoundOrInvalidTokenError,
    #[from(ignore)]
    LinkCharacterToGroupError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetCharacterGroupLinkError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetGroupAdminError(tokio_postgres::error::Error),
    #[from(ignore)]
    UpdateAccountUsernameError(tokio_postgres::error::Error),
    #[from(ignore)]
    UpdateAccountPasswordError(tokio_postgres::error::Error),
    #[from(ignore)]
    DeleteAccountError(tokio_postgres::error::Error),
    AccountHasNoPasswordSetError,
    IncorrectCurrentPasswordError,
    #[from(ignore)]
    GetGroupPermissionsError(tokio_postgres::error::Error),
    #[from(ignore)]
    UpdateGroupPermissionsError(tokio_postgres::error::Error),
    #[from(ignore)]
    EnsureOpenSessionError(tokio_postgres::error::Error),
    #[from(ignore)]
    CloseIdleSessionsError(tokio_postgres::error::Error),
    #[from(ignore)]
    InsertActivityEventError(tokio_postgres::error::Error),
    #[from(ignore)]
    ListActivityEventsError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetProgressSnapshotError(tokio_postgres::error::Error),
    #[from(ignore)]
    ListSessionsError(tokio_postgres::error::Error),
    #[from(ignore)]
    ListKillEventsError(tokio_postgres::error::Error),
    CannotModifyGroupAdminPermissionsError,
    AccountAuthRequiredError,
    PermissionDeniedError,
    #[from(ignore)]
    UpsertPushSubscriptionError(tokio_postgres::error::Error),
    #[from(ignore)]
    DeletePushSubscriptionError(tokio_postgres::error::Error),
    #[from(ignore)]
    ListPushSubscriptionsError(tokio_postgres::error::Error),
    PushNotConfiguredError,
    #[from(ignore)]
    GetDiscordWebhookSettingsError(tokio_postgres::error::Error),
    #[from(ignore)]
    UpdateDiscordWebhookSettingsError(tokio_postgres::error::Error),
    #[from(ignore)]
    DiscordWebhookInvalidError(String),
    #[from(ignore)]
    CaptureLeaderboardSnapshotsError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetLeaderboardSnapshotsError(tokio_postgres::error::Error),
    #[from(ignore)]
    ResolveMemberForAccountError(tokio_postgres::error::Error),
    #[from(ignore)]
    UpdateMemberColorError(tokio_postgres::error::Error),
    InvalidMemberColorError,
    MemberColorTargetNotFoundError,
    #[from(ignore)]
    MemberColorTakenError(String),
    MetricDataXpNotSupportedError,
    CannotRemoveSelfError,
    CannotBlockSelfError,
    AccountLockedError,
    MustChangePasswordError,
    #[from(ignore)]
    GetItemBonusesError(tokio_postgres::error::Error),
    #[from(ignore)]
    UpsertItemBonusesError(tokio_postgres::error::Error),
    #[from(ignore)]
    ItemBonusesScrapeError(String),
    #[from(ignore)]
    GetActivitySettingsError(tokio_postgres::error::Error),
    #[from(ignore)]
    UpdateActivitySettingsError(tokio_postgres::error::Error),
    ActivityEventNotFoundError,
    ActivityReactionsDisabledError,
    InvalidReactionTypeError,
    #[from(ignore)]
    ToggleActivityReactionError(tokio_postgres::error::Error),
    #[from(ignore)]
    GetActivityReactionsError(tokio_postgres::error::Error),
    #[from(ignore)]
    ListActivityCommentsError(tokio_postgres::error::Error),
    #[from(ignore)]
    AddActivityCommentError(tokio_postgres::error::Error),
    #[from(ignore)]
    ActivityCommentValidationError(String),
    ActivityCommentLimitReachedError,
    CommentRequiresLinkedCharacterError,
    #[from(ignore)]
    ListChatMessagesError(tokio_postgres::error::Error),
    #[from(ignore)]
    AddChatMessageError(tokio_postgres::error::Error),
    ChatRequiresLinkedCharacterError,
    #[from(ignore)]
    ChatMessageValidationError(String),
    ChatRateLimited,
}
impl std::error::Error for ApiError {}
fn handle_pg_error(err: &tokio_postgres::error::Error, name: &str) -> HttpResponse {
    match err.as_db_error() {
        Some(db_error) => log::error!("{}: {}", name, db_error.message()),
        None => log::error!("{}: {}", name, err),
    };

    HttpResponse::InternalServerError().finish()
}
impl ResponseError for ApiError {
    fn error_response(&self) -> HttpResponse {
        match *self {
            ApiError::PoolError(ref err) => {
                log::error!("PoolError: {}", err);
                HttpResponse::InternalServerError().body(format!("PoolError: {}", err))
            }
            ApiError::GroupCreationError(ref err) => handle_pg_error(err, "GroupCreationError"),
            ApiError::UpdateGroupMemberError(ref err) => {
                handle_pg_error(err, "UpdateGroupMemberError")
            }
            ApiError::PGError(ref err) => handle_pg_error(err, "PGError"),
            ApiError::GetGroupError(ref err) => handle_pg_error(err, "GetGroupError"),
            ApiError::AddMemberError(ref err) => handle_pg_error(err, "AddMemberError"),
            ApiError::GetGroupDataError(ref err) => handle_pg_error(err, "GetGroupDataError"),
            ApiError::IsMemberInGroupError(ref err) => handle_pg_error(err, "IsMemberInGroupError"),
            ApiError::GetSkillsDataError(ref err) => handle_pg_error(err, "GetSkillsDataError"),
            ApiError::GetMemberColorsError(ref err) => handle_pg_error(err, "GetMemberColorsError"),
            ApiError::UpsertMemberMeshError(ref err) => {
                handle_pg_error(err, "UpsertMemberMeshError")
            }
            ApiError::GetMemberMeshError(ref err) => handle_pg_error(err, "GetMemberMeshError"),
            ApiError::DeleteGroupMemberError(ref err) => {
                handle_pg_error(err, "DeleteGroupMemberError")
            }
            ApiError::RenameGroupError(ref err) => handle_pg_error(err, "RenameGroupError"),
            ApiError::IsMemberBlockedError(ref err) => handle_pg_error(err, "IsMemberBlockedError"),
            ApiError::BlockGroupMemberError(ref err) => {
                handle_pg_error(err, "BlockGroupMemberError")
            }
            ApiError::UnblockGroupMemberError(ref err) => {
                handle_pg_error(err, "UnblockGroupMemberError")
            }
            ApiError::GetBlockedMembersError(ref err) => {
                handle_pg_error(err, "GetBlockedMembersError")
            }
            ApiError::MemberBlockedError => {
                HttpResponse::Forbidden().body("This player has been blocked from the group")
            }
            ApiError::DemoGroupReadOnlyError => {
                HttpResponse::Forbidden().body("The demo group is read-only")
            }
            ApiError::SeedError(ref message) => {
                log::error!("SeedError: {}", message);
                HttpResponse::InternalServerError().finish()
            }
            ApiError::RerollGroupTokenError(ref err) => {
                handle_pg_error(err, "RerollGroupTokenError")
            }
            ApiError::DeleteGroupError(ref err) => handle_pg_error(err, "DeleteGroupError"),
            ApiError::SerdeJsonError(ref err) => {
                log::error!("SerdeJsonError: {}", err);
                HttpResponse::InternalServerError().body(format!("SerdeJsonError: {}", err))
            }
            ApiError::GroupFullError => HttpResponse::BadRequest()
                .body("Group has already reached the maximum amount of players"),
            ApiError::UreqError(ref err) => {
                log::error!("UreqError: {}", err);
                HttpResponse::InternalServerError().body(format!("UreqError: {}", err))
            }
            ApiError::GroupMemberValidationError(ref reason) => {
                log::error!("Validation error: {}", reason);
                HttpResponse::BadRequest().body(reason.clone())
            }
            ApiError::AdminDbError(ref context, ref err) => handle_pg_error(err, context),
            ApiError::AdminNotFoundError => HttpResponse::NotFound().finish(),
            ApiError::AdminRateLimitedError => HttpResponse::TooManyRequests().finish(),
            ApiError::AdminInvalidRequestError(ref reason) => {
                HttpResponse::BadRequest().body(reason.clone())
            }
            ApiError::CreateAccountError(ref err) => handle_pg_error(err, "CreateAccountError"),
            ApiError::UsernameAlreadyRegisteredError => {
                HttpResponse::Conflict().body("Username already registered")
            }
            ApiError::GetAccountError(ref err) => handle_pg_error(err, "GetAccountError"),
            ApiError::CreateAccountSessionError(ref err) => {
                handle_pg_error(err, "CreateAccountSessionError")
            }
            ApiError::InvalidCredentialsError => {
                HttpResponse::Unauthorized().body("Invalid username or password")
            }
            ApiError::AccountDisabledError => HttpResponse::Forbidden().body("Account is disabled"),
            ApiError::DiscordOAuthError(ref reason) => {
                log::error!("DiscordOAuthError: {}", reason);
                HttpResponse::BadGateway().body("Discord login failed")
            }
            ApiError::DiscordIdAlreadyLinkedError => HttpResponse::Conflict()
                .body("That Discord account is already linked to a different GroupScape account"),
            ApiError::CreateCharacterError(ref err) => handle_pg_error(err, "CreateCharacterError"),
            ApiError::GetCharacterError(ref err) => handle_pg_error(err, "GetCharacterError"),
            ApiError::DeleteCharacterError(ref err) => handle_pg_error(err, "DeleteCharacterError"),
            ApiError::CharacterDenylistedError => HttpResponse::Conflict()
                .body("This character was removed from your account and can't be re-linked."),
            ApiError::CharacterCapReachedError => {
                HttpResponse::Forbidden().body("Character cap reached")
            }
            ApiError::CharacterNotFoundError => {
                HttpResponse::NotFound().body("Character not found")
            }
            ApiError::CharacterAlreadyInGroupError => HttpResponse::Conflict()
                .body("Character already belongs to a group. Leave that group first."),
            ApiError::GroupNotFoundOrInvalidTokenError => {
                HttpResponse::Unauthorized().body("Group not found or token is invalid")
            }
            ApiError::LinkCharacterToGroupError(ref err) => {
                handle_pg_error(err, "LinkCharacterToGroupError")
            }
            ApiError::GetCharacterGroupLinkError(ref err) => {
                handle_pg_error(err, "GetCharacterGroupLinkError")
            }
            ApiError::GetGroupAdminError(ref err) => handle_pg_error(err, "GetGroupAdminError"),
            ApiError::UpdateAccountUsernameError(ref err) => {
                handle_pg_error(err, "UpdateAccountUsernameError")
            }
            ApiError::UpdateAccountPasswordError(ref err) => {
                handle_pg_error(err, "UpdateAccountPasswordError")
            }
            ApiError::DeleteAccountError(ref err) => handle_pg_error(err, "DeleteAccountError"),
            ApiError::AccountHasNoPasswordSetError => {
                HttpResponse::BadRequest().body("This account has no password set")
            }
            ApiError::IncorrectCurrentPasswordError => {
                HttpResponse::Unauthorized().body("Current password is incorrect")
            }
            ApiError::GetGroupPermissionsError(ref err) => {
                handle_pg_error(err, "GetGroupPermissionsError")
            }
            ApiError::UpdateGroupPermissionsError(ref err) => {
                handle_pg_error(err, "UpdateGroupPermissionsError")
            }
            ApiError::EnsureOpenSessionError(ref err) => {
                handle_pg_error(err, "EnsureOpenSessionError")
            }
            ApiError::CloseIdleSessionsError(ref err) => {
                handle_pg_error(err, "CloseIdleSessionsError")
            }
            ApiError::InsertActivityEventError(ref err) => {
                handle_pg_error(err, "InsertActivityEventError")
            }
            ApiError::ListActivityEventsError(ref err) => {
                handle_pg_error(err, "ListActivityEventsError")
            }
            ApiError::GetProgressSnapshotError(ref err) => {
                handle_pg_error(err, "GetProgressSnapshotError")
            }
            ApiError::ListSessionsError(ref err) => handle_pg_error(err, "ListSessionsError"),
            ApiError::ListKillEventsError(ref err) => handle_pg_error(err, "ListKillEventsError"),
            ApiError::CannotModifyGroupAdminPermissionsError => HttpResponse::Conflict()
                .body("The group admin's permissions are implicit and cannot be changed"),
            ApiError::AccountAuthRequiredError => HttpResponse::Unauthorized()
                .body("This action requires a logged-in account (X-Account-Authorization)"),
            ApiError::PermissionDeniedError => {
                HttpResponse::Forbidden().body("You do not have permission to perform this action")
            }
            ApiError::UpsertPushSubscriptionError(ref err) => {
                handle_pg_error(err, "UpsertPushSubscriptionError")
            }
            ApiError::DeletePushSubscriptionError(ref err) => {
                handle_pg_error(err, "DeletePushSubscriptionError")
            }
            ApiError::ListPushSubscriptionsError(ref err) => {
                handle_pg_error(err, "ListPushSubscriptionsError")
            }
            ApiError::PushNotConfiguredError => {
                HttpResponse::ServiceUnavailable().body("Web push is not configured on this server")
            }
            ApiError::GetDiscordWebhookSettingsError(ref err) => {
                handle_pg_error(err, "GetDiscordWebhookSettingsError")
            }
            ApiError::UpdateDiscordWebhookSettingsError(ref err) => {
                handle_pg_error(err, "UpdateDiscordWebhookSettingsError")
            }
            ApiError::DiscordWebhookInvalidError(ref reason) => HttpResponse::BadRequest()
                .body(format!("Discord webhook URL is invalid: {}", reason)),
            ApiError::CaptureLeaderboardSnapshotsError(ref err) => {
                handle_pg_error(err, "CaptureLeaderboardSnapshotsError")
            }
            ApiError::GetLeaderboardSnapshotsError(ref err) => {
                handle_pg_error(err, "GetLeaderboardSnapshotsError")
            }
            ApiError::ResolveMemberForAccountError(ref err) => {
                handle_pg_error(err, "ResolveMemberForAccountError")
            }
            ApiError::UpdateMemberColorError(ref err) => {
                handle_pg_error(err, "UpdateMemberColorError")
            }
            ApiError::InvalidMemberColorError => {
                HttpResponse::BadRequest().body("Not a valid helmet colour")
            }
            ApiError::MemberColorTargetNotFoundError => HttpResponse::NotFound()
                .body("This account has no character in this group yet"),
            ApiError::MemberColorTakenError(ref taken_by) => {
                HttpResponse::Conflict().body(format!("That colour is already worn by {taken_by}"))
            }
            ApiError::MetricDataXpNotSupportedError => HttpResponse::BadRequest()
                .body("Xp is not supported on get-metric-data; use get-skill-data instead"),
            ApiError::CannotRemoveSelfError => {
                HttpResponse::BadRequest().body("You can't remove yourself from the group")
            }
            ApiError::CannotBlockSelfError => {
                HttpResponse::BadRequest().body("You can't block yourself from the group")
            }
            ApiError::AccountLockedError => HttpResponse::build(actix_web::http::StatusCode::LOCKED)
                .body("Too many failed login attempts. Try again in 15 minutes."),
            ApiError::MustChangePasswordError => HttpResponse::Forbidden()
                .body("You must change your password before continuing"),
            ApiError::GetItemBonusesError(ref err) => handle_pg_error(err, "GetItemBonusesError"),
            ApiError::UpsertItemBonusesError(ref err) => {
                handle_pg_error(err, "UpsertItemBonusesError")
            }
            ApiError::ItemBonusesScrapeError(ref reason) => {
                log::warn!("ItemBonusesScrapeError: {}", reason);
                HttpResponse::BadGateway().body("Failed to fetch item bonuses from the OSRS Wiki")
            }
            ApiError::GetActivitySettingsError(ref err) => {
                handle_pg_error(err, "GetActivitySettingsError")
            }
            ApiError::UpdateActivitySettingsError(ref err) => {
                handle_pg_error(err, "UpdateActivitySettingsError")
            }
            ApiError::ActivityEventNotFoundError => {
                HttpResponse::NotFound().body("Activity event not found")
            }
            ApiError::ActivityReactionsDisabledError => HttpResponse::BadRequest()
                .body("Likes and comments are turned off for this group"),
            ApiError::InvalidReactionTypeError => {
                HttpResponse::BadRequest().body("Not a valid reaction type")
            }
            ApiError::ToggleActivityReactionError(ref err) => {
                handle_pg_error(err, "ToggleActivityReactionError")
            }
            ApiError::GetActivityReactionsError(ref err) => {
                handle_pg_error(err, "GetActivityReactionsError")
            }
            ApiError::ListActivityCommentsError(ref err) => {
                handle_pg_error(err, "ListActivityCommentsError")
            }
            ApiError::AddActivityCommentError(ref err) => {
                handle_pg_error(err, "AddActivityCommentError")
            }
            ApiError::ActivityCommentValidationError(ref reason) => {
                HttpResponse::BadRequest().body(reason.clone())
            }
            ApiError::ActivityCommentLimitReachedError => HttpResponse::BadRequest()
                .body("This item already has the maximum of 10 comments"),
            ApiError::CommentRequiresLinkedCharacterError => HttpResponse::BadRequest()
                .body("Link a character to this group before commenting"),
            ApiError::ListChatMessagesError(ref err) => {
                handle_pg_error(err, "ListChatMessagesError")
            }
            ApiError::AddChatMessageError(ref err) => handle_pg_error(err, "AddChatMessageError"),
            ApiError::ChatRequiresLinkedCharacterError => HttpResponse::BadRequest()
                .body("Link a character to this group before sending chat messages"),
            ApiError::ChatMessageValidationError(ref reason) => {
                HttpResponse::BadRequest().body(reason.clone())
            }
            ApiError::ChatRateLimited => HttpResponse::TooManyRequests()
                .body("Too many chat messages - slow down and try again shortly"),
        }
    }
}
