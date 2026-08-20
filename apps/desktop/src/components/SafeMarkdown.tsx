import Markdown, { type Components } from "react-markdown";
import remarkBreaks from "remark-breaks";
import remarkGfm from "remark-gfm";

const components: Components = {
  a: ({ children }) => <span className="aizu-markdown__link">{children}</span>,
  img: ({ alt }) => alt ? <span className="aizu-markdown__image-alt">{alt}</span> : null,
  table: ({ children }) => (
    <div className="aizu-markdown__table-wrap">
      <table>{children}</table>
    </div>
  ),
};

export function SafeMarkdown({ children }: { children: string }) {
  return (
    <div className="aizu-banner__body aizu-markdown">
      <Markdown
        components={components}
        remarkPlugins={[remarkGfm, remarkBreaks]}
        skipHtml
      >
        {children}
      </Markdown>
    </div>
  );
}
