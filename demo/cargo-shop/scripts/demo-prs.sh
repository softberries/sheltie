#!/usr/bin/env bash
# Opens the demo's agent-style pull requests against a clone of the demo repository.
#
#   scripts/demo-prs.sh <clone>            push branches and open pull requests (needs gh)
#   scripts/demo-prs.sh <clone> --local    only create the branches locally
#
# Each scenario is one branch off main; see README.md for what Sheltie does.
set -euo pipefail

REPO="${1:?usage: demo-prs.sh <clone> [--local]}"
MODE="${2:-}"
cd "$REPO"
git fetch -q origin main 2>/dev/null || true
BASE=$(git rev-parse main)

branch() { # name title body
  git checkout -q -B "$1" "$BASE"
}

commit_and_open() { # name title body
  git add -A
  git -c user.name="demo-agent" -c user.email="demo-agent@users.noreply.github.com" commit -q -m "$2"
  if [ "$MODE" != "--local" ]; then
    git push -q -f origin "$1"
    gh pr create --head "$1" --base main --title "$2" --body "$3" >/dev/null
    echo "opened: $2"
  else
    echo "branch: $1"
  fi
}

# --- Independent changes: one per crate --------------------------------------------------
branch demo/search-prefix
cat >> crates/search/src/lib.rs <<'RS'

/// Products whose SKU starts with `prefix`.
pub fn by_sku_prefix(prefix: &str) -> Vec<Product> {
    products().into_iter().filter(|p| p.sku.starts_with(prefix)).collect()
}
RS
commit_and_open demo/search-prefix "Search: find products by SKU prefix" "Adds \`by_sku_prefix\` to search."

branch demo/auth-expiry
cat >> crates/auth/src/lib.rs <<'RS'

/// Whether a session created at `created` (seconds) has expired at `now`, after 30 days.
pub fn expired(created: u64, now: u64) -> bool {
    now.saturating_sub(created) > 30 * 24 * 3600
}
RS
commit_and_open demo/auth-expiry "Auth: expire sessions after 30 days" "Adds \`expired\` to auth."

branch demo/catalog-hoodie
sed -i.bak 's/        Product { sku: "STK-01", name: "Sticker pack", price: 400 },/        Product { sku: "STK-01", name: "Sticker pack", price: 400 },\n        Product { sku: "HOD-01", name: "Shepherd hoodie", price: 4_900 },/' crates/catalog/src/lib.rs && rm crates/catalog/src/lib.rs.bak
commit_and_open demo/catalog-hoodie "Catalog: add the shepherd hoodie" "Adds a product."

# --- Feature flags: both append to the same file ----------------------------------------
branch demo/flag-dark-mode
echo "dark_mode" >> config/feature_flags.txt
commit_and_open demo/flag-dark-mode "Flags: dark mode" "Adds the \`dark_mode\` flag."

branch demo/flag-express
echo "express_checkout" >> config/feature_flags.txt
commit_and_open demo/flag-express "Flags: express checkout" "Adds the \`express_checkout\` flag."

# --- Migrations: both add number 0002 ----------------------------------------------------
branch demo/migration-orders
printf 'CREATE TABLE orders (id BIGINT PRIMARY KEY, total BIGINT NOT NULL);\n' > migrations/0002_orders.sql
commit_and_open demo/migration-orders "Migrations: orders table" "Adds \`0002_orders.sql\`."

branch demo/migration-customers
printf 'CREATE TABLE customers (id BIGINT PRIMARY KEY, email TEXT NOT NULL);\n' > migrations/0002_customers.sql
commit_and_open demo/migration-customers "Migrations: customers table" "Adds \`0002_customers.sql\`."

# --- Semantic conflict: a rename, and a new caller of the old name -----------------------
branch demo/rename-rate
sed -i.bak 's/pub fn rate() -> u32/pub fn standard_rate() -> u32/; s/u64::from(rate())/u64::from(standard_rate())/' crates/pricing/src/lib.rs && rm crates/pricing/src/lib.rs.bak
sed -i.bak 's/shop_pricing::rate()/shop_pricing::standard_rate()/' crates/invoice/src/lib.rs && rm crates/invoice/src/lib.rs.bak
commit_and_open demo/rename-rate "Pricing: rename rate() to standard_rate()" "Makes room for reduced rates."

branch demo/reduced-rate
cat >> crates/pricing/src/lib.rs <<'RS'

/// The reduced VAT rate for books and food: half the standard rate.
pub fn reduced_rate() -> u32 {
    rate() / 2
}

#[cfg(test)]
mod reduced_tests {
    #[test]
    fn reduced_is_half() {
        assert_eq!(super::reduced_rate(), 10);
    }
}
RS
commit_and_open demo/reduced-rate "Pricing: reduced VAT rate" "Adds the reduced rate for books and food.

\`\`\`sheltie-intent
{
  \"task\": \"Add reduced_rate() to shop-pricing: half the standard VAT rate.\",
  \"plan\": \"Define reduced_rate() in crates/pricing/src/lib.rs in terms of the standard rate, with a unit test.\",
  \"acceptance\": [\"reduced_tests::reduced_is_half\"]
}
\`\`\`"

git checkout -q main
