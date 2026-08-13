export const formatTimestamp = (
  value: string | null,
  locale?: string,
  unavailable = "Time unavailable",
): string => {
  if (value === null) {
    return "No events yet";
  }

  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return unavailable;
  }

  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
};

export const humanize = (value: string): string =>
  value
    .replace(/([A-Z])/gu, " $1")
    .replace(/^./u, (letter) => letter.toUpperCase());
