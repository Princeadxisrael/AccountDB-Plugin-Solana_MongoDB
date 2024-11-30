use {log::*, std::collections::HashSet};

#[derive(Debug, Default)]
pub(crate) struct AccountsSelector {
    pub accounts: HashSet<Vec<u8>>,
    pub owners: HashSet<Vec<u8>>,
    pub select_all_accounts: bool,
}

impl AccountsSelector {
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
            .filter_map(|key| {
                bs58::decode(key).into_vec().ok() // Use filter_map to handle errors
            })
            .collect();
        let owners = owners
            .iter()
            .filter_map(|key| {
                bs58::decode(key).into_vec().ok() // Use filter_map to handle errors
            })
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

    pub fn is_enabled(&self) -> bool {
        self.select_all_accounts || !self.accounts.is_empty() || !self.owners.is_empty()
    }
}

fn main() {
    // Initialize the logger
    env_logger::init();
    // Test case 1: Normal accounts and owners
    let accounts = vec!["ABC123".to_string(), "DEF456".to_string()];
    let owners = vec!["oWNER789".to_string()];
    let selector = AccountsSelector::new(&accounts, &owners);
    info!("Selector 1: {:?}", selector);
    info!("Is enabled: {}", selector.is_enabled());

    // Test case 2: Select all accounts
    let accounts = vec!["*".to_string()];
    let owners = vec![];
    let selector = AccountsSelector::new(&accounts, &owners);
    info!("Selector 2: {:?}", selector);
    info!("Is enabled: {}", selector.is_enabled());

    // Test case 3: Empty accounts and owners
    let accounts = vec![];
    let owners = vec![];
    let selector = AccountsSelector::new(&accounts, &owners);
    info!("Selector 3: {:?}", selector);
    info!("Is enabled: {}", selector.is_enabled());

    // Test is_account_selected
    let account = bs58::decode("ABC123").into_vec().unwrap();
    let owner = bs58::decode("oWNER789").into_vec().unwrap();
    info!("Is account selected: {}", selector.is_account_selected(&account, &owner));
}