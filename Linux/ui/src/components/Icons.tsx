// Minimal stroke icon set (24x24, currentColor).
import { SVGProps } from "react";

type P = SVGProps<SVGSVGElement> & { size?: number };

function Svg({ size = 18, children, ...rest }: P & { children: React.ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...rest}
    >
      {children}
    </svg>
  );
}

export const IconScreens = (p: P) => (
  <Svg {...p}>
    <rect x="2" y="4" width="9" height="7" rx="1.5" />
    <rect x="13" y="4" width="9" height="7" rx="1.5" />
    <rect x="7.5" y="14" width="9" height="6" rx="1.5" />
  </Svg>
);
export const IconMonitor = (p: P) => (
  <Svg {...p}>
    <rect x="2.5" y="3.5" width="19" height="13" rx="2" />
    <path d="M8 20.5h8M12 16.5v4" />
  </Svg>
);
export const IconLaptop = (p: P) => (
  <Svg {...p}>
    <rect x="4" y="4.5" width="16" height="11" rx="1.5" />
    <path d="M2 19.5h20" />
  </Svg>
);
export const IconDevices = (p: P) => (
  <Svg {...p}>
    <rect x="2.5" y="4" width="13" height="10" rx="1.5" />
    <path d="M6 18h6" />
    <rect x="17.5" y="8" width="4" height="12" rx="1" />
  </Svg>
);
export const IconTransfer = (p: P) => (
  <Svg {...p}>
    <path d="M7 4v14M3.5 14.5 7 18l3.5-3.5" />
    <path d="M17 20V6M13.5 9.5 17 6l3.5 3.5" />
  </Svg>
);
export const IconSettings = (p: P) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
  </Svg>
);
export const IconPulse = (p: P) => (
  <Svg {...p}>
    <path d="M3 12h4l3-8 4 16 3-8h4" />
  </Svg>
);
export const IconShield = (p: P) => (
  <Svg {...p}>
    <path d="M12 2.5 4 5.5v6c0 5 3.4 8.6 8 10 4.6-1.4 8-5 8-10v-6z" />
    <path d="m9 12 2 2 4-4" />
  </Svg>
);
export const IconEye = (p: P) => (
  <Svg {...p}>
    <path d="M1.5 12S5.5 5 12 5s10.5 7 10.5 7-4 7-10.5 7S1.5 12 1.5 12z" />
    <circle cx="12" cy="12" r="3" />
  </Svg>
);
export const IconX = (p: P) => (
  <Svg {...p}>
    <path d="M18 6 6 18M6 6l12 12" />
  </Svg>
);
export const IconCheck = (p: P) => (
  <Svg {...p}>
    <path d="m4.5 12.5 5 5 10-11" />
  </Svg>
);
export const IconPlus = (p: P) => (
  <Svg {...p}>
    <path d="M12 5v14M5 12h14" />
  </Svg>
);
export const IconUpload = (p: P) => (
  <Svg {...p}>
    <path d="M12 16V4M7 9l5-5 5 5" />
    <path d="M4 15v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3" />
  </Svg>
);
export const IconFolder = (p: P) => (
  <Svg {...p}>
    <path d="M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2.5h8.5A1.5 1.5 0 0 1 21 9v9.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z" />
  </Svg>
);
export const IconTrash = (p: P) => (
  <Svg {...p}>
    <path d="M4 7h16M10 11v6M14 11v6M6 7l1 12.5A1.5 1.5 0 0 0 8.5 21h7a1.5 1.5 0 0 0 1.5-1.5L18 7M9 7V4.5A1.5 1.5 0 0 1 10.5 3h3A1.5 1.5 0 0 1 15 4.5V7" />
  </Svg>
);
export const IconRefresh = (p: P) => (
  <Svg {...p}>
    <path d="M20 11a8 8 0 0 0-14.6-4.5L4 8M4 4v4h4" />
    <path d="M4 13a8 8 0 0 0 14.6 4.5L20 16M20 20v-4h-4" />
  </Svg>
);
export const IconCursor = (p: P) => (
  <Svg {...p}>
    <path d="m5 3 14 7-6 2-2 6z" />
  </Svg>
);
export const IconAlert = (p: P) => (
  <Svg {...p}>
    <path d="M10.3 3.9 2.4 17.5A2 2 0 0 0 4.1 20.5h15.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" />
    <path d="M12 9v4M12 17h.01" />
  </Svg>
);
export const IconInfo = (p: P) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="9.5" />
    <path d="M12 11v5M12 8h.01" />
  </Svg>
);
export const IconLock = (p: P) => (
  <Svg {...p}>
    <rect x="4.5" y="10.5" width="15" height="10" rx="2" />
    <path d="M8 10.5V7a4 4 0 0 1 8 0v3.5" />
  </Svg>
);
export const IconUnlock = (p: P) => (
  <Svg {...p}>
    <rect x="4.5" y="10.5" width="15" height="10" rx="2" />
    <path d="M8 10.5V7a4 4 0 0 1 7.7-1.5" />
  </Svg>
);
export const IconLink = (p: P) => (
  <Svg {...p}>
    <path d="M10 14a4.5 4.5 0 0 0 6.4 0l3-3a4.5 4.5 0 0 0-6.4-6.4l-1 1" />
    <path d="M14 10a4.5 4.5 0 0 0-6.4 0l-3 3a4.5 4.5 0 0 0 6.4 6.4l1-1" />
  </Svg>
);
export const IconWifi = (p: P) => (
  <Svg {...p}>
    <path d="M2 8.5a15 15 0 0 1 20 0M5 12a10 10 0 0 1 14 0M8.5 15.5a5 5 0 0 1 7 0" />
    <path d="M12 19h.01" />
  </Svg>
);
export const IconKeyboard = (p: P) => (
  <Svg {...p}>
    <rect x="2" y="5.5" width="20" height="13" rx="2" />
    <path d="M6 9.5h.01M10 9.5h.01M14 9.5h.01M18 9.5h.01M6 13h.01M18 13h.01M10 13h4M7 16h10" />
  </Svg>
);
export const IconClipboard = (p: P) => (
  <Svg {...p}>
    <rect x="5" y="4" width="14" height="17" rx="2" />
    <rect x="9" y="2.5" width="6" height="3.5" rx="1" />
  </Svg>
);
export const IconGauge = (p: P) => (
  <Svg {...p}>
    <path d="M3.5 17a9 9 0 1 1 17 0" />
    <path d="m12 13 4-5" />
    <circle cx="12" cy="13.5" r="1.2" />
  </Svg>
);
export const IconPower = (p: P) => (
  <Svg {...p}>
    <path d="M12 3v8" />
    <path d="M6.3 6.8a8 8 0 1 0 11.4 0" />
  </Svg>
);
export const IconWindows = (p: P) => (
  <Svg {...p} strokeWidth={0} fill="currentColor">
    <path d="M3 5.2 10.5 4v7.3H3zm8.5-1.4L21 2.5v8.8h-9.5zM3 12.7h7.5V20L3 18.8zm8.5 0H21v8.8l-9.5-1.3z" />
  </Svg>
);
export const IconLinux = (p: P) => (
  <Svg {...p}>
    <path d="M12 3c-2 0-3 1.8-3 4 0 1.2-.4 2.2-1.2 3.3C6.6 12 5.5 14 6 16.2c.3 1.4 1.4 2 2.5 2.3M12 3c2 0 3 1.8 3 4 0 1.2.4 2.2 1.2 3.3 1.2 1.7 2.3 3.7 1.8 5.9-.3 1.4-1.4 2-2.5 2.3" />
    <path d="M8 18.5c1 .9 2.4 1.5 4 1.5s3-.6 4-1.5M5 19.5c1.2.8 2.2 1 3 .3M19 19.5c-1.2.8-2.2 1-3 .3" />
    <path d="M10.5 7.5h.01M13.5 7.5h.01M11 10h2" />
  </Svg>
);
