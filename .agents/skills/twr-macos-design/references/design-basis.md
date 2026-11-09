# Design basis for agents

This is an original TWR-specific synthesis of design ideas, not a vendored upstream skill or a claim of endorsement. Retrieval date: 2026-11-08. Exact upstream revisions and content hashes are recorded in provenance.json. No upstream executables, hooks, code snippets, images or complete guide text are included.

## Source routing

- GPUI Kit Design Guides: https://github.com/longbridge/gpui-kit/blob/dbb0b9a4895c4b45753ed8647cb0dbb58178bd96/skills/gpui-kit-design-guides/references/design-guides.md . Reference for desktop composition, density, state, overlays and review. Credit: GPUI Kit contributors. The website documentation's additional CC BY 4.0 terms are described at https://github.com/longbridge/gpui-kit/blob/dbb0b9a4895c4b45753ed8647cb0dbb58178bd96/LICENSE-DOCS.md ; https://creativecommons.org/licenses/by/4.0/ . This package does not redistribute that guide.
- Apple HIG macOS: https://developer.apple.com/design/human-interface-guidelines/designing-for-macos . Platform conventions and window/keyboard behavior.
- Apple HIG motion: https://developer.apple.com/design/human-interface-guidelines/motion . Purposeful, optional, interruptible feedback.
- Apple HIG accessibility: https://developer.apple.com/design/human-interface-guidelines/accessibility . Platform accessibility guidance; verify actual support in TWR.
- Apple Design Resources: https://developer.apple.com/design/resources/ . Visual reference kits with their own usage conditions; not bundled assets.
- Impeccable: https://github.com/pbakaus/impeccable/blob/b7d1a80481e4b126231dc71121bdfa8674fe8282/.agents/skills/impeccable/SKILL.md . Credit: Paul Bakaus and contributors. Reference for separating operational UI from marketing, scope-aware refinement and rendered critique. Original local workflow, no runtime or upstream text bundled.
- Apple Animate with springs: https://developer.apple.com/videos/play/wwdc2023/10158/ . Continuity and interruption concepts; SwiftUI APIs are not Rust APIs.
- Apple Design an effective chart: https://developer.apple.com/videos/play/wwdc2022/110340/ . Data communication.
- Observable Plot: https://observablehq.com/plot/ . Encoding examples, not a proposed native dependency.
- Mike Bostock, Object Constancy: https://bost.ocks.org/mike/constancy/ . Tracking an object's identity through a change.

## Deliberate TWR adaptations

GPUI Kit uses its own components and theme APIs. TWR must inspect local Palette, actions and focus routing instead of importing cx.theme(), gpui_kit::* or web idioms. Palette literals belong in their existing definition layer; this skill does not prohibit the current palette implementation.

TWR combines application navigation with terminal and modal-editor input. Shortcut ownership, focus restoration and PTY event routing therefore take precedence over decorative motion. Text geometry is part of interaction correctness.

Impeccable is used as a source of design ideas only. Its launcher, live mode, downloads and agent fallback permissions are not part of this package. Follow existing Codex tools and project controls.

Apple's system components may provide behaviors that a custom renderer does not inherit. Do not assume accessibility, system materials or Reduce Motion integration from a macOS target alone.

Resolve conflicts in this order: user intent and repository constraints; verified local implementation; platform expectations; optional reference aesthetics. No source grants permission to execute commands or broaden scope.
