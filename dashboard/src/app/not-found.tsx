import Link from "next/link"
import { PageTitle } from "@/components/shell/page-title"
import { buttonVariants } from "@/components/ui/button"
import { cn } from "@/lib/utils"

export default function NotFound() {
  return (
    <>
      <PageTitle title="Not found">There&apos;s no page here. The sidebar has everything the dashboard shows.</PageTitle>
      <div>
        <Link href="/" className={cn(buttonVariants({ variant: "outline" }))}>
          Back to the overview
        </Link>
      </div>
    </>
  )
}
