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

// Antialiased text over a translucent background, rastered into a layer above other layers and composited over them,
// rounds a channel up to one level away from the same text painted straight over what is below it.
const ROUNDING_OF_TEXT_OVER_A_TRANSLUCENT_BACKGROUND_IN_A_LAYER = 1;

function printPresentedFrameState(state, tolerance = 0) {
    println(`compared: ${state.compared}, matches a direct raster: ${state.maxChannelDifference <= tolerance}`);
}

async function presentedFrameStateOnceNothingIsLeftToRasterize() {
    let state = await presentedFrameState();
    for (let attempt = 0; attempt < 10 && state.tilesRasterizedSinceLastCheck !== 0; ++attempt)
        state = await presentedFrameState();
    return state;
}

// The layers the display list dump names, one per line, with the media and backdrop filters that got no layer.
function layersOfTheDisplayList() {
    const dump = internals.dumpDisplayList();
    const start = dump.indexOf("Layers:\n") + "Layers:\n".length;
    return dump
        .slice(start, dump.indexOf("\n\n", start))
        .split("\n")
        .map(line => line.trim())
        .join("\n");
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

// The layers the display list dump draws the fill of `color` into, each as the `Layers:` section names it.
function layersOfTheFillOfColor(color) {
    const fill = internals
        .dumpDisplayList()
        .split("\n")
        .find(line => / FillRect@/.test(line) && line.includes(`color=${color}`));
    if (!fill) return `no fill of ${color}`;
    const layerLines = layersOfTheDisplayList().split("\n");
    return fill
        .split(" ")[0]
        .split(",")
        .map(layer => layerLines.find(line => line.startsWith(`[${layer}] `)) ?? layer)
        .join(", ");
}
