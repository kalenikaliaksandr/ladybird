function animationFrame() {
    return new Promise(resolve => requestAnimationFrame(() => resolve()));
}

// The compositor's comparison of the frame it presented last with its display list painted straight into one surface,
// once a frame rendered after the call has reached the screen.
async function presentedFrameState() {
    await animationFrame();
    await animationFrame();
    for (let attempt = 0; attempt < 100; ++attempt) {
        const state = internals.presentedFrameState();
        if (state.compared) return state;
        await animationFrame();
    }
    return internals.presentedFrameState();
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

function colorUniqueToThisPageLoad() {
    const stamp = Math.floor(performance.timeOrigin + performance.now());
    return `rgb(${stamp & 255}, ${(stamp >> 8) & 255}, ${(stamp >> 16) & 255})`;
}
