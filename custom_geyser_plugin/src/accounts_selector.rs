
use {log::*, std::collections::HashSet};

#[derive(Debug, Default)]
pub(crate) struct AccountsSelector {
    pub accounts: HashSet<Vec<u8>>,
    pub owners: HashSet<Vec<u8>>,
    pub select_all_accounts: bool,
}


impl AccountsSelector {
    //initialize new accounts and owners
    pub fn new(accounts: &[String], owners: &[String]) -> Self {
        info!(
            "Creating AccountsSelector from accounts: {:?}, owners: {:?}",
            accounts, owners
        );

        let select_all_accounts = accounts.iter().any(|key| key == "*");
        if select_all_accounts {
            return AccountsSelector {
                accounts: HashSet::default(),
                owners: HashSet::default(),
                select_all_accounts,
            };
        }
        let accounts = accounts
            .iter()
            .map(|key| bs58::decode(key).into_vec().unwrap())
            .collect();
        let owners = owners
            .iter()
            .map(|key| bs58::decode(key).into_vec().unwrap())
            .collect();
        AccountsSelector {
            accounts,
            owners,
            select_all_accounts,
        }
    }

   pub fn is_account_selected(&self, account: &[u8], owner: &[u8]) -> bool {
        self.select_all_accounts || self.accounts.contains(account) || self.owners.contains(owner)
    }

    /// Check if any account is of interested at all
    pub fn is_enabled(&self) -> bool {
        self.select_all_accounts || !self.accounts.is_empty() || !self.owners.is_empty()
    }
}