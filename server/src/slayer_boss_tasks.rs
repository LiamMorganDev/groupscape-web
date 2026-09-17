use std::collections::HashSet;
use std::sync::LazyLock;

/// Every boss assignable via the generic "Boss" slayer task (unlocked by the 200-point "Like a
/// boss" reward, given by Duradel/Kuradal, Konar, Nieve/Steve, and Krystilia) - mirrors the site's
/// `slayer.js` TASK_ICON map (the ~35 boss entries at the bottom of it). Kept as a separate list
/// rather than sharing one across the Rust/JS boundary since there's no existing mechanism for
/// that; kept in sync manually - see also the JS copy's own comment.
static BOSS_TASK_NAMES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "the leviathan",
        "the whisperer",
        "vardorvis",
        "duke sucellus",
        "abyssal sire",
        "alchemical hydra",
        "cerberus",
        "thermonuclear smoke devil",
        "kraken",
        "grotesque guardians",
        "dagannoth rex",
        "dagannoth prime",
        "dagannoth supreme",
        "kalphite queen",
        "giant mole",
        "sarachnis",
        "k'ril tsutsaroth",
        "kree'arra",
        "commander zilyana",
        "general graardor",
        "vet'ion",
        "callisto",
        "venenatis",
        "scorpia",
        "chaos elemental",
        "chaos fanatic",
        "crazy archaeologist",
        "king black dragon",
        "vorkath",
        "zulrah",
        "phantom muspah",
        "araxxor",
    ])
});

/// The list as owned, lowercase strings for binding into a SQL `= ANY($n)` parameter.
pub fn names() -> Vec<String> {
    BOSS_TASK_NAMES.iter().map(|n| n.to_string()).collect()
}

/// Whether `task_name` (any case) is one of the generic-"Boss"-task assignments - matches how
/// the site's `slayerData.taskIconUrl` normalizes before looking up its own copy of this list.
pub fn is_boss_task(task_name: &str) -> bool {
    BOSS_TASK_NAMES.contains(task_name.trim().to_lowercase().as_str())
}
