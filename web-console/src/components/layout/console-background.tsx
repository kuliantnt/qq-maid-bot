/** 背景装饰层：默认模式只呈现主题底色；
 * 特殊九宫格与自定义背景的激活由 data-background 属性驱动（background controller 写入）。
 * 背景层 aria-hidden，不承载信息、不可交互。 */
export function ConsoleBackground() {
  return (
    <>
      <div className="console-background console-background--special" aria-hidden="true">
        <div className="console-background-grid">
          {Array.from({ length: 9 }, (_, index) => (
            <div key={index} className="console-background-grid-cell" />
          ))}
        </div>
      </div>
      <div className="console-background console-background--custom" aria-hidden="true" />
    </>
  );
}
