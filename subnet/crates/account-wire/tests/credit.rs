//! `billing-policy/v1`'s draw rule: the one place the quote and the hold
//! agree on what a check takes from the plan and from credit.

use slop_ninja_account_wire::credit::{Draw, PAYG_MIN_TOKENS, PLAN_MIN_TOKENS, draw, holds_on};
use slop_ninja_account_wire::detect_page::Channel;

const PAGE: Channel = Channel::PaidPage;

#[test]
fn credit_alone_bills_at_least_the_minimum() {
    assert_eq!(draw(50, PAGE, 0, 0, 1000), Some(Draw { plan: 0, credit: PAYG_MIN_TOKENS }));
    assert_eq!(draw(2_000, PAGE, 0, 0, 2_000), Some(Draw { plan: 0, credit: 2_000 }));
    assert_eq!(draw(2_000, PAGE, 0, 0, 1_999), None, "short by one token");
    assert_eq!(draw(0, PAGE, 0, 0, 127), None, "even an empty check is billed the minimum");
}

#[test]
fn the_plan_draws_its_minimum_while_its_room_allows() {
    // A short document takes the plan's minimum.
    assert_eq!(draw(200, PAGE, 10_000, 0, 0), Some(Draw { plan: PLAN_MIN_TOKENS, credit: 0 }));
    // A long one takes its own tokens.
    assert_eq!(draw(3_000, PAGE, 10_000, 0, 0), Some(Draw { plan: 3_000, credit: 0 }));
    // The plan's last tokens cover what they can; credit the rest.
    assert_eq!(draw(3_000, PAGE, 1_000, 0, 5_000), Some(Draw { plan: 1_000, credit: 2_000 }));
    // Room between the document and the plan's minimum: the room, no credit.
    assert_eq!(draw(200, PAGE, 300, 0, 0), Some(Draw { plan: 300, credit: 0 }));
    // No credit, and the plan cannot cover it: short.
    assert_eq!(draw(3_000, PAGE, 1_000, 0, 0), None);
}

#[test]
fn the_api_draws_within_the_api_part_only() {
    assert_eq!(draw(3_000, Channel::Api, 10_000, 1_000, 5_000), Some(Draw { plan: 1_000, credit: 2_000 }));
    assert_eq!(draw(3_000, Channel::Pro, 10_000, 1_000, 0), Some(Draw { plan: 3_000, credit: 0 }));
}

#[test]
fn every_draw_covers_the_billed_tokens() {
    for tokens in [0u64, 1, 127, 128, 511, 512, 513, 2_046, 40_000] {
        for plan_left in [0u64, 1, 300, 512, 600, 100_000] {
            for credit_left in [0u64, 100, 128, 1_000, 100_000] {
                let billed = tokens.max(PAYG_MIN_TOKENS);
                match draw(tokens, PAGE, plan_left, 0, credit_left) {
                    Some(d) => {
                        assert!(d.total() >= billed, "{tokens} {plan_left} {credit_left}");
                        assert!(d.plan <= plan_left && d.credit <= credit_left);
                        assert!(d.credit == 0 || d.plan == plan_left, "credit only after the plan's room");
                    }
                    None => assert!(plan_left.saturating_add(credit_left) < billed),
                }
            }
        }
    }
}

#[test]
fn holds_are_placed_on_the_paid_channels_only() {
    assert!(holds_on(Channel::PaidPage) && holds_on(Channel::Pro) && holds_on(Channel::Api));
    assert!(!holds_on(Channel::FreeText) && !holds_on(Channel::FreeUrl) && !holds_on(Channel::Operator));
}
