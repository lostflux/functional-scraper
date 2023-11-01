#!/usr/bin/env python3
# -*- coding: utf-8 -*-

"""
  Simple script to generate metadata about corpus.
"""

__author__  = "Amittai Siavava"
__version__ = "0.0.1"

from os import mkdir
from collections import Counter
import csv
import pandas as pd

def count_words():
  """
    This is a simple script to count the number of words in this directory.
    
    It loops over all the lines in `.all` and counts the occurrence of each word,
    then sums them up.

    HACK:
    This is a hacky way to count the number of words in the corpus.
    >>> count_words()
  """
  total = 0
  with open("all", "r") as f:
    for line in f:
      try:
        total += int(line.strip().split()[0])
      except:
        pass
    f.close()
  if total > 0:
    with open (".total", "w") as f:
      print(f"Total words = {total}")
      f.write(f"Total words = {total}")
      f.close()

def index_pages():
  """
    Generate a friendly index of the pages.

    We create a csv and a tsv (in case one proves more convenient than the other).
  """
  docID = 0

  with open("index.csv", "w") as csv_file, open("urls", "w") as urls:
    writer = csv.writer(csv_file)
    # csv.write("id,year,title,url\n")
    writer.writerow(["id", "year", "title", "url", "text"])
    while True:
      try:
        with open(f"../log/{docID}", "r") as meta, open(f"../log/{docID}.txt", "r") as data:
          title = meta.readline().strip()
          year = meta.readline().strip()
          url = meta.readline().strip()
          # read remaining text
          text = data.read()
          # print(f"{text = }")



          meta.close()
          data.close()

          print(f"Indexing: {docID}")
          # csv.write(f'{docID},{year},"{title}","{url}","{text}"\n')
          
          writer.writerow([docID, year, f'"{title}"', f'"{url}"', f'"{text}"'])
          # tsv.write(f"{docID}\t{year}\t{title}\t{url}\n")
          urls.write(f"{url}\n")
          docID += 1
      except:
        break
  print("Done.")

def categorize():
  """
    Categorize the pages by year.
  """

  docID = 0
  years = Counter()
  while True:
    try:
      with open(f"../log/{docID}.txt", "r") as doc, open(f"../log/{docID}", "r") as meta:
        title = meta.readline().strip()
        year = meta.readline().strip()
        url = meta.readline().strip()
        text = doc.read()
        doc.close()
        meta.close()

        if year == "":
          year = "unknown"

        try:
          mkdir(f"../categorized/{year}")
        except:
          pass

        id = years.get(year, 0)
        with open(f"../categorized/{year}/{id}.txt", "w") as f:
          f.write(f"old id = {docID}\n{title}\n{year}\n{url}\n\n{text}")
          f.close()
        years[year] = id + 1
        docID += 1
        
    except:
      break

# def load_data():
#   df = pd.read_csv("index.csv")
#   df.head(5)


if __name__ == "__main__":
  # count_words()
  index_pages()
  categorize()
  # load_data()
