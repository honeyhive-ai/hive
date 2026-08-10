// `react-native-gesture-handler` must be the first import in the app entry —
// the drawer's pan gestures do not register otherwise.
import "react-native-gesture-handler";
import { registerRootComponent } from "expo";

import App from "./App";

registerRootComponent(App);
