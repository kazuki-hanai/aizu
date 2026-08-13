import appIconUrl from "../../src-tauri/icons/128x128.png";

type BrandMarkProps = {
  small?: boolean;
};

export function BrandMark({ small = false }: BrandMarkProps) {
  return (
    <img
      alt=""
      aria-hidden="true"
      className={`brand-mark${small ? " brand-mark--small" : ""}`}
      draggable={false}
      src={appIconUrl}
    />
  );
}
