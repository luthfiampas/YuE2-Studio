/// <reference types="vite/client" />

// Declare .txt file imports as raw text
declare module '*.txt' {
  const content: string;
  export default content;
}

declare module '*.txt?raw' {
  const content: string;
  export default content;
}

declare module '*.md?raw' {
  const content: string;
  export default content;
}

declare const __APP_VERSION__: string;
declare const __GIT_COMMIT__: string;
