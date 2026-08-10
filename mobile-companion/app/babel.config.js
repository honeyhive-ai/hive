module.exports = function (api) {
  api.cache(true);
  return {
    presets: ["babel-preset-expo"],
    // Reanimated's plugin rewrites worklets and has to run last. The drawer
    // navigator depends on it; without this the drawer throws at runtime
    // rather than at build time, which is a confusing failure.
    plugins: ["react-native-reanimated/plugin"],
  };
};
