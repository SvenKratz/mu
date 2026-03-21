# Lunar Lander 2D Web Game — Product Brief

## Overview

A browser-based 2D lunar lander game built with HTML5 Canvas and vanilla JavaScript. The player controls a spacecraft descending toward a mountainous lunar surface, managing thrust, fuel, and velocity to achieve a safe landing on a designated landing zone.

## Core Mechanics

### Physics Model
- **Gravity**: Constant lunar gravity (~1.62 m/s²) applied each frame
- **Thrust**: Player-controlled upward and lateral thrust that opposes gravity
- **Fuel**: Finite fuel supply; thrust consumes fuel proportionally. When fuel is depleted, thrust is disabled
- **Velocity**: Spacecraft starts with an initial velocity (downward + lateral). Landing velocity must be below a safe threshold to succeed

### Spacecraft State
- Position (x, y)
- Velocity (vx, vy)
- Rotation / orientation
- Fuel remaining (displayed to player)
- Thrust on/off + direction

### Lunar Surface
- Procedurally generated mountainous terrain using line segments or a heightmap
- One or more flat designated **landing zones** highlighted on the surface
- Landing zone width should be challenging but fair

### Win / Lose Conditions
- **Successful landing**: spacecraft touches down on the landing zone with vertical speed below threshold, horizontal speed below threshold, and roughly level orientation
- **Crash**: contact with terrain outside the landing zone, or landing zone contact at excessive speed/angle

## User Controls

| Action         | Key(s)             |
|----------------|--------------------|
| Main thrust    | Up Arrow / W       |
| Rotate left    | Left Arrow / A     |
| Rotate right   | Right Arrow / D    |
| Restart        | R                  |

## HUD Display

- Altitude
- Horizontal velocity
- Vertical velocity
- Fuel gauge (numeric + bar)
- Landing status indicator (safe speed = green, danger = red)
- Score (based on fuel remaining + landing precision)

## Sound Effects

Generate or source simple sound effects for:
- Main thruster firing (looping while thrust active)
- Successful landing (celebratory tone)
- Crash (explosion / impact)
- Low fuel warning (alarm beep)
- UI feedback (restart, menu interactions)

Sound can be generated programmatically using the Web Audio API (oscillators, noise buffers) or from short audio files.

## Tech Stack

- **Rendering**: HTML5 `<canvas>` 2D context
- **Language**: Vanilla JavaScript (ES modules OK)
- **Audio**: Web Audio API for procedural sound effects
- **Structure**: Single `index.html` entry point, or `index.html` + JS/CSS files served statically
- **No frameworks** — keep dependencies minimal

## Testing — Playwright

End-to-end tests using Playwright to verify:

1. **App launches** — page loads without console errors, canvas renders
2. **Controls respond** — simulating key presses produces visible spacecraft state changes (position, rotation, fuel decrement)
3. **Landing mission completable** — automated sequence that pilots the lander to a successful landing (scripted key inputs timed to achieve safe touchdown)
4. **Crash detection works** — verify crash state triggers on high-speed impact
5. **Fuel depletion** — verify thrust disables when fuel reaches zero
6. **HUD updates** — altitude, velocity, fuel values change over time

## Acceptance Criteria

1. App launches in browser without errors
2. Has user controls (keyboard input moves/rotates spacecraft, controls thrust)
3. User can successfully complete a landing mission
4. Sound effects play for thrust, landing, and crash events
5. HUD displays real-time flight telemetry
6. Playwright test suite passes all cases above

## Project Decomposition

This project should be broken down into subtasks suitable for kanban tracking. The recommended decomposition:

### Planning Tasks (spawn first)
- [ ] **PLAN: Physics engine** — Define gravity, thrust, fuel consumption model; decide on timestep and units
- [ ] **PLAN: Terrain generation** — Design algorithm for mountainous surface + landing zone placement
- [ ] **PLAN: Rendering pipeline** — Plan canvas drawing order, camera/viewport, coordinate system
- [ ] **PLAN: Sound design** — Specify Web Audio API synthesis approach for each sound effect
- [ ] **PLAN: Test strategy** — Define Playwright test scenarios and scripted landing sequence

### Implementation Tasks
- [ ] **IMPL: Project scaffold** — Create `index.html`, canvas setup, game loop (`requestAnimationFrame`)
- [ ] **IMPL: Physics engine** — Gravity, thrust, velocity integration, fuel consumption
- [ ] **IMPL: Spacecraft rendering** — Draw lander sprite/shape, thrust flame animation
- [ ] **IMPL: Terrain generation** — Procedural mountainous heightmap with flat landing zone(s)
- [ ] **IMPL: Terrain rendering** — Draw lunar surface, landing zone markers, starfield background
- [ ] **IMPL: Collision detection** — Spacecraft vs. terrain contact, landing zone check
- [ ] **IMPL: Win/lose logic** — Evaluate landing speed, angle, position; display outcome
- [ ] **IMPL: HUD overlay** — Altitude, velocity, fuel gauge, status indicators
- [ ] **IMPL: User controls** — Keyboard input handling (thrust, rotation, restart)
- [ ] **IMPL: Sound effects** — Web Audio API synthesis for thrust, landing, crash, warnings
- [ ] **IMPL: Game state management** — Start, playing, landed, crashed, restart flow
- [ ] **IMPL: Score system** — Calculate and display score on successful landing

### Testing & Polish Tasks
- [ ] **TEST: Playwright setup** — Install Playwright, configure test project, static server
- [ ] **TEST: App launch** — Verify page loads, canvas renders, no console errors
- [ ] **TEST: Controls** — Verify keyboard input produces expected state changes
- [ ] **TEST: Landing mission** — Scripted automated successful landing
- [ ] **TEST: Crash & fuel** — Verify crash detection and fuel depletion
- [ ] **POLISH: Tuning** — Balance gravity, thrust power, fuel amount, landing zone size for fun gameplay
- [ ] **POLISH: Visual effects** — Particle effects for thrust/crash, smooth animations
