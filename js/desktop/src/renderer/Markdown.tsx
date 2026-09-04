import { Streamdown } from "streamdown";
export default function Markdown({
  text,
  streaming,
}: {
  text: string;
  streaming: boolean;
}) {
  return (
    <Streamdown mode={streaming ? "streaming" : "static"}>{text}</Streamdown>
  );
}
