use anchor_lang::prelude::*;

#[error_code]
pub enum NaimError {
    #[msg("Name is already registered")]
    NameAlreadyRegistered,
    #[msg("Caller is not the name authority")]
    Unauthorized,
    #[msg("Name has expired")]
    NameExpired,
    #[msg("Name is permanent and cannot be renewed or released this way")]
    NamePermanent,
    #[msg("Name is invalid")]
    InvalidName,
    #[msg("Metadata URI is too long")]
    MetadataUriTooLong,
    #[msg("Too many linked wallets")]
    TooManyLinkedWallets,
    #[msg("Wallet is already linked")]
    WalletAlreadyLinked,
    #[msg("Invalid configuration parameters")]
    InvalidConfig,
    #[msg("No rewards to claim")]
    NothingToClaim,
    #[msg("Stake is still locked")]
    StakeLocked,
    #[msg("Insufficient staked balance")]
    InsufficientStake,
    #[msg("Category is invalid (must be like label.agent)")]
    InvalidCategory,
    #[msg("Name is not directly under this category")]
    NotUnderCategory,
    #[msg("Listing price must be greater than zero")]
    InvalidPrice,
    #[msg("You cannot buy your own listing")]
    SelfPurchase,
    #[msg("The listing's seller no longer owns the name")]
    SellerNotOwner,
    #[msg("Unknown listing currency")]
    InvalidCurrency,
    #[msg("Wrong buy instruction for this listing's currency")]
    CurrencyMismatch,
    #[msg("Payment mint does not match the listing's currency")]
    WrongPayMint,
}
