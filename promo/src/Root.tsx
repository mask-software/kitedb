import { Composition } from "remotion";
import { KiteDBPromo } from "./KiteDBPromo";

export const RemotionRoot: React.FC = () => {
  return (
    <Composition
      id="KiteDBPromo"
      component={KiteDBPromo}
      durationInFrames={900} // 30 seconds at 30fps
      fps={30}
      width={1920}
      height={1080}
    />
  );
};
