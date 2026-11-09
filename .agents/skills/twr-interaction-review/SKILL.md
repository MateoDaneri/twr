---
name: twr-interaction-review
description: Review Terminal Workspace UI flows, visual polish, focus, resizing and motion in the real macOS GPUI app. Use after interaction changes or when designing transitions; does not perform a general security audit or authorize builds.
---

# TWR interaction review

Review the requested flow using synthetic project and terminal content. Identify the exact source revision and running candidate before attributing visible behavior to a change. Respect paused goals and project build safeguards; this skill does not authorize resuming a goal or compiling an app.

## Inspect the task in use

Read the relevant view, event routing and existing palette. Observe the primary action and its result in the real app when available. Assess issues in terms of their effect: losing input, hiding an action, obscuring state, wasting work area, or making reading harder. Keep aesthetic preference separate from a functional defect.

Check relevant cases, rather than running a universal exhaustive suite for a one-line change:

- Keyboard and pointer routes reach the same outcome. Focus is visible and stays with the intended editor, PTY, list or overlay.
- Opening and cancelling an overlay restores the expected selection, scroll and focus. Closing a split resolves focus to a valid remaining pane.
- Resize preserves usable controls and coherent alignment at the smallest supported size and a normal working size. Check device-scale rounding when changing drawing or geometry.
- Light/dark appearance, long labels, empty/error states and disabled actions remain legible where relevant.
- An unavailable action explains its state. User-facing errors identify a recovery action without exposing internal implementation unnecessarily.

## Specify each changed transition

Record the trigger, persistent object identity, start and end state, animated properties, completion condition, interruption/reversal behavior, and reduced-motion alternative. Choose timing for the actual transition; do not impose arbitrary global durations.

Do not delay typing, command dispatch or focus assignment to wait for a decorative animation. Repeated input must not queue an unbounded series of transitions. Preserve selection and hit-test consistency while a panel moves or resizes.

Verify how the current GPUI version and macOS bridge expose motion preferences. Do not assume a method in upstream main exists locally. If support is missing, report the gap and use a nonanimated path for the affected feature rather than claiming Reduce Motion works.

After completion or dismissal, inspect whether decorative work still schedules frames. Distinguish intended terminal content animation from an unnecessary app-chrome animation loop.

## Critique, fix, verify

Prioritize a few observed defects by impact, correct them within scope, then inspect the complete flow again. Judge hierarchy and alignment across regions, not only individual controls. Preserve factual content and product behavior during polish.

When the user permits automated tests, consider `$gpui-test` for meaningful state/event regressions; otherwise use only the explicitly permitted review methods. For a performance claim or a suspected cost introduced by motion, use the existing `$gpui-bench`, `$instruments-profiling` and `$native-app-performance` workflows as applicable. Follow their bounded runs and candidate checks; do not start expensive builds merely to complete a visual checklist.

Check native accessibility through the actual accessibility tree and supported assistive interaction when the change affects custom controls. DOM/ARIA rules and screenshot contrast alone do not prove VoiceOver support.

Report what was observed, how it was exercised, and what remains unverified. A mockup is design evidence; a screenshot is visual evidence; a recording supports interaction evidence; a benchmark supports only the measured performance claim. Do not substitute one for another.

Design sources and adaptations: [design basis](../twr-macos-design/references/design-basis.md). This local companion skill is included in the same package; no external runtime is needed.
