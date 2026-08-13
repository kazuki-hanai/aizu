import {
  AlertTriangle,
  CheckCircle2,
  CircleDashed,
  CircleOff,
  type LucideIcon,
} from "lucide-react";

type Tone = "success" | "warning" | "danger" | "muted";

type StatusBadgeProps = {
  icon?: LucideIcon;
  label: string;
  tone: Tone;
};

const toneIcon: Record<Tone, LucideIcon> = {
  success: CheckCircle2,
  warning: CircleDashed,
  danger: AlertTriangle,
  muted: CircleOff,
};

export function StatusBadge({ icon, label, tone }: StatusBadgeProps) {
  const Icon = icon ?? toneIcon[tone];

  return (
    <span className={`status-badge status-badge--${tone}`}>
      <Icon aria-hidden="true" size={14} strokeWidth={2} />
      {label}
    </span>
  );
}
