import { useSeoMeta } from "@unhead/react";
import { Link } from "react-router-dom";

import { Card, CardContent } from "@/components/ui/card";

const NotFound = () => {
  useSeoMeta({
    title: "404 — npanel",
    description: "The page you are looking for could not be found.",
  });

  return (
    <Card>
      <CardContent className="py-12 text-center space-y-3">
        <h1 className="text-3xl font-semibold">404</h1>
        <p className="text-muted-foreground">That page does not exist.</p>
        <Link to="/" className="text-sm underline">
          Back to sites
        </Link>
      </CardContent>
    </Card>
  );
};

export default NotFound;
