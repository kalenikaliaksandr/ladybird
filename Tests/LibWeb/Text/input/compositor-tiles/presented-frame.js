function animationFrame() {
    return new Promise(resolve => requestAnimationFrame(() => resolve()));
}

// Animation frames of tests come as fast as the event loop allows, while a frame can take tens of milliseconds to reach
// the screen when many tests run at once, and the highlights of the compositor layers overlay keep frames on their way
// for half a second. So the wait is bounded by time rather than by a count of frames.
const LONGEST_WAIT_FOR_A_PRESENTED_FRAME_IN_MILLISECONDS = 5000;

// The compositor's comparison of the frame it presented last with its display list painted straight into one surface,
// once a frame rendered after the call has reached the screen, or the state as it is once that took too long, which
// says nothing was compared.
async function presentedFrameState() {
    await animationFrame();
    await animationFrame();
    const deadline = performance.now() + LONGEST_WAIT_FOR_A_PRESENTED_FRAME_IN_MILLISECONDS;
    for (;;) {
        const state = internals.presentedFrameState();
        if (state.compared || performance.now() >= deadline) return state;
        await animationFrame();
    }
}

function printPresentedFrameState(state, tolerance = 0) {
    println(`compared: ${state.compared}, matches a direct raster: ${state.maxChannelDifference <= tolerance}`);
}

async function presentedFrameStateOnceNothingIsLeftToRasterize() {
    let state = await presentedFrameState();
    for (let attempt = 0; attempt < 10 && state.tilesRasterizedSinceLastCheck !== 0; ++attempt)
        state = await presentedFrameState();
    return state;
}

// The compositor layers overlay of the frame painted last, without the lines that depend on when it was painted.
function compositorLayersWithoutTiming() {
    return internals
        .compositorLayers()
        .split("\n")
        .filter(line => line !== "" && !line.startsWith("last frame:") && !line.startsWith("highlighted:"))
        .join("\n");
}

function colorUniqueToThisPageLoad() {
    const stamp = Math.floor(performance.timeOrigin + performance.now());
    return `rgb(${stamp & 255}, ${(stamp >> 8) & 255}, ${(stamp >> 16) & 255})`;
}
