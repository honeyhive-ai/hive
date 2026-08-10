module.exports = function (api) {
  api.cache(true);
  return {
    presets: ["babel-preset-expo"],
    // The worklet-rewriting plugin has to run last. The drawer navigator
    // depends on it; without this the drawer throws at runtime rather than at
    // build time, which is a confusing failure.
    //
    // Reanimated 4 split worklets into their own package, so the plugin moved
    // from `react-native-reanimated/plugin` to `react-native-worklets/plugin`.
    plugins: ["react-native-worklets/plugin"],
  };
};
