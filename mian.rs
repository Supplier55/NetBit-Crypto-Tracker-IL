mod api;
mod models;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use slint::{ModelRc, VecModel};

use api::prices::{fetch_prices, fetch_usd_ils_rate, fallback_prices};
use models::portfolio::{Portfolio, combo_to_symbol, coin_color};
use models::tax::{calculate_tax, format_nis, format_usd};
use models::report::{generate_bank_report, ReportData};
use models::csv_import::{import_csv, transactions_to_holdings};
use rfd;

slint::include_modules!();

// Shared app state
struct AppState {
    portfolio: Portfolio,
    prices: HashMap<String, f64>,
    usd_ils_rate: f64,
    last_report_path: Option<String>,
}

impl AppState {
    fn new() -> Self {
        Self {
            portfolio: Portfolio::new(),
            prices: fallback_prices(),
            usd_ils_rate: 3.68,
            last_report_path: None,
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    std::env::set_var("SLINT_BACKEND", "software");
    let ui = AppWindow::new()?;
    let state = Arc::new(Mutex::new(AppState::new()));

    // ── Initial price fetch ─────────────────────────────────────────────────
    {
        let state_clone = state.clone();
        let ui_handle = ui.as_weak();
        std::thread::spawn(move || {
            let prices = fetch_prices().unwrap_or_else(|_| fallback_prices());
            let rate = fetch_usd_ils_rate().unwrap_or(3.68);

            {
                let mut s = state_clone.lock().unwrap();
                s.prices = prices;
                s.usd_ils_rate = rate;
            }

            let _ = ui_handle.upgrade_in_event_loop(move |ui| {
                let rate_str = {
                    let s = state_clone.lock().unwrap();
                    format!("$1 = ₪{:.2}", s.usd_ils_rate)
                };
                ui.set_usd_ils_rate(rate_str.into());
                ui.set_last_updated(chrono::Local::now().format("%H:%M:%S").to_string().into());
            });
        });
    }

    // ── Refresh Prices ──────────────────────────────────────────────────────
    {
        let state_clone = state.clone();
        let ui_handle = ui.as_weak();
        ui.on_refresh_prices(move || {
            let state_inner = state_clone.clone();
            let ui_h = ui_handle.clone();
            std::thread::spawn(move || {
                let prices = fetch_prices().unwrap_or_else(|_| fallback_prices());
                let rate = fetch_usd_ils_rate().unwrap_or(3.68);

                {
                    let mut s = state_inner.lock().unwrap();
                    s.prices = prices;
                    s.usd_ils_rate = rate;
                }

                let _ = ui_h.upgrade_in_event_loop(move |ui| {
                    let s = state_inner.lock().unwrap();
                    ui.set_usd_ils_rate(format!("$1 = ₪{:.2}", s.usd_ils_rate).into());
                    ui.set_last_updated(
                        chrono::Local::now().format("%H:%M:%S").to_string().into()
                    );
                    drop(s);
                    update_portfolio_ui(&ui, &state_inner);
                });
            });
        });
    }

    // ── Add Coin ────────────────────────────────────────────────────────────
    {
        let state_clone = state.clone();
        let ui_handle = ui.as_weak();
        ui.on_add_coin(move |combo_val, amount_str, buy_price_str| {
            let amount: f64 = amount_str.trim().parse().unwrap_or(0.0);
            let buy_price: f64 = buy_price_str.trim().parse().unwrap_or(0.0);
            if amount <= 0.0 || buy_price <= 0.0 { return; }

            let (symbol, name) = combo_to_symbol(&combo_val);
            {
                let mut s = state_clone.lock().unwrap();
                s.portfolio.add_or_update(
                    symbol.to_string(), name.to_string(), amount, buy_price
                );
            }

            if let Some(ui) = ui_handle.upgrade() {
                update_portfolio_ui(&ui, &state_clone);
            }
        });
    }

    // ── Remove Coin ─────────────────────────────────────────────────────────
    {
        let state_clone = state.clone();
        let ui_handle = ui.as_weak();
        ui.on_remove_coin(move |index| {
            {
                let mut s = state_clone.lock().unwrap();
                s.portfolio.remove(index as usize);
            }
            if let Some(ui) = ui_handle.upgrade() {
                update_portfolio_ui(&ui, &state_clone);
            }
        });
    }

    // ── Calculate Tax ───────────────────────────────────────────────────────
    {
        let state_clone = state.clone();
        let ui_handle = ui.as_weak();
        ui.on_calculate_tax(move |buy_str, sell_str, qty_str, tax_type| {
            let buy: f64 = buy_str.trim().parse().unwrap_or(0.0);
            let sell: f64 = sell_str.trim().parse().unwrap_or(0.0);
            let qty: f64 = qty_str.trim().parse().unwrap_or(1.0);
            let is_business = tax_type.contains("47");
            let rate = { state_clone.lock().unwrap().usd_ils_rate };

            if buy <= 0.0 || sell <= 0.0 { return; }

            let result = calculate_tax(buy, sell, qty, rate, is_business);

            if let Some(ui) = ui_handle.upgrade() {
                ui.set_tax_calculated(true);
                ui.set_tax_is_loss(result.is_loss);
                ui.set_tax_profit_usd(format_usd(result.profit_usd).into());
                ui.set_tax_profit_nis(format_nis(result.profit_nis).into());
                ui.set_tax_amount(
                    format!("{} ({:.0}%)", format_nis(result.tax_amount_nis), result.tax_rate * 100.0).into()
                );
                ui.set_tax_net(format_nis(result.net_profit_nis).into());
            }
        });
    }

    // ── Generate Bank Report ────────────────────────────────────────────────
    {
        let state_clone = state.clone();
        let ui_handle = ui.as_weak();
        ui.on_generate_report(move |name, id, year, bank, source| {
            let s = state_clone.lock().unwrap();
            let total_usd = s.portfolio.total_value_usd(&s.prices);
            let total_nis = total_usd * s.usd_ils_rate;
            let coin_count = s.portfolio.holdings.len();
            let rate = s.usd_ils_rate;
            drop(s);

            let data = ReportData {
                full_name: name.to_string(),
                id_number: id.to_string(),
                tax_year: year.to_string(),
                bank_name: bank.to_string(),
                source: source.to_string(),
                total_nis,
                total_usd,
                coin_count,
                usd_ils_rate: rate,
            };

            match generate_bank_report(&data) {
                Ok(path) => {
                    state_clone.lock().unwrap().last_report_path = Some(path.clone());
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_report_status("הדוח נוצר בהצלחה! לחץ לפתיחה".into());
                        ui.set_report_ready(true);
                    }
                }
                Err(e) => {
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_report_status(format!("שגיאה: {}", e).into());
                    }
                }
            }
        });
    }

    // ── Export PDF (open HTML in browser) ──────────────────────────────────
    {
        let state_clone = state.clone();
        ui.on_export_pdf(move || {
            let path = state_clone.lock().unwrap().last_report_path.clone();
            if let Some(p) = path {
                let _ = open::that(&p);
            }
        });
    }

    // ── Import CSV ──────────────────────────────────────────────────────────
    {
        let state_clone = state.clone();
        let ui_handle = ui.as_weak();
        ui.on_import_csv(move || {
            // Native file picker dialog
            let picked = rfd::FileDialog::new()
                .add_filter("CSV Files", &["csv"])
                .set_title("בחר קובץ CSV לייבוא")
                .pick_file();

            let path = match picked {
                Some(p) => p.to_string_lossy().to_string(),
                None => return,
            };

            match import_csv(&path) {
                Ok(txns) => {
                    let holdings = transactions_to_holdings(txns);
                    {
                        let mut s = state_clone.lock().unwrap();
                        for h in holdings {
                            s.portfolio.add_or_update(
                                h.symbol.clone(),
                                h.name.clone(),
                                h.amount,
                                h.buy_price_usd,
                            );
                        }
                    }
                    if let Some(ui) = ui_handle.upgrade() {
                        update_portfolio_ui(&ui, &state_clone);
                    }
                }
                Err(e) => eprintln!("CSV import error: {}", e),
            }
        });
    }

    ui.run()
}

/// Update the portfolio section of the UI from current state
fn update_portfolio_ui(ui: &AppWindow, state: &Arc<Mutex<AppState>>) {
    let s = state.lock().unwrap();
    let prices = &s.prices;
    let rate = s.usd_ils_rate;

    // Build coin rows
    let coins: Vec<_> = s.portfolio.holdings.iter().map(|h| {
        let val_usd = h.current_value_usd(prices);
        let val_nis = val_usd * rate;
        let pnl = h.pnl_pct(prices);
        let is_pos = pnl >= 0.0;
        let pnl_sign = if is_pos { "+" } else { "" };

        CoinData {
            symbol: h.symbol.clone().into(),
            name: h.name.clone().into(),
            amount: format!("{:.4} {}", h.amount, h.symbol).into(),
            value_nis: format_nis(val_nis).into(),
            value_usd: format_usd(val_usd).into(),
            pnl: format!("{}{:.1}%", pnl_sign, pnl).into(),
            is_positive: is_pos,
            coin_color: coin_color(&h.symbol),
        }
    }).collect();

    let total_usd = s.portfolio.total_value_usd(prices);
    let total_nis = total_usd * rate;
    let pnl_usd = s.portfolio.total_pnl_usd(prices);
    let pnl_nis = pnl_usd * rate;
    let pnl_pct = s.portfolio.total_pnl_pct(prices);
    let pnl_pos = pnl_usd >= 0.0;
    let pnl_sign = if pnl_pos { "+" } else { "" };
    let count = s.portfolio.holdings.len();

    drop(s);

    let model = ModelRc::new(VecModel::from(coins));
    ui.set_coins(model);
    ui.set_total_nis(format_nis(total_nis).into());
    ui.set_total_usd(format_usd(total_usd).into());
    ui.set_total_pnl(format!("{}{}", pnl_sign, format_nis(pnl_nis)).into());
    ui.set_total_pnl_pct(format!("{}{:.1}%", pnl_sign, pnl_pct).into());
    ui.set_pnl_positive(pnl_pos);
    ui.set_coin_count(count as i32);
}
