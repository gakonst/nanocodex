import { Container } from "@cloudflare/containers";

export class ChatGptEgress extends Container {
  defaultPort = 8080;
  enableInternet = true;
  sleepAfter = "1h";
}
