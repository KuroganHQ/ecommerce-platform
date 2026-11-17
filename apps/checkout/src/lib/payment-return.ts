export const returnedFromProvider = (search: string) => {
  const query = new URLSearchParams(search);
  return query.get("redirect_status") === "succeeded" || query.get("returned") === "1";
};

export const afterNewAttempt = (href: string) => {
  const url = new URL(href);
  return `${url.origin}${url.pathname}`;
};
